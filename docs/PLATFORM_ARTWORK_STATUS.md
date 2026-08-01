# Platform artwork status

Audit date: 2026-08-01. Canonical platform source:
`archivefs_core::platform::PLATFORMS`. This document is a presentation manifest,
not a second platform registry.

## Convention and requirements

The preferred filename is the canonical platform ID reduced to lowercase ASCII
alphanumerics, followed by `.png`. It is stable across display-name changes and
is never guessed from a title, manufacturer name, or substring. For example,
the persisted IDs `PSX`, `PS2`, and `PS3` resolve to `psx.png`, `ps2.png`, and
`ps3.png`.

New source artwork must be 1024x1024 PNG on a square 1:1 canvas, use a
transparent background where practical, show the whole machine in a centred
three-quarter product view with comfortable padding, and contain no added text,
border, scenery, people, or characters. Artwork is not resized, recompressed,
or converted as part of this audit.

The GUI resolution order is deterministic:

1. `<canonical-id>.png` from the explicitly configured custom directory.
2. The exact canonical bundled PNG, when one exists.
3. An intentional category custom PNG, when configured.
4. The native category glyph corresponding to the documented SVG fallback.
5. The native Unknown glyph.

## Complete canonical-platform audit

`Code` means the canonical key is resolved in code. `Bundled` means the PNG is
compiled with `include_bytes!`; `key only` means a correctly named custom image
can be used but no platform-specific PNG is bundled. All bundled images are
valid, non-animated 1024x1024 8-bit RGBA PNGs containing transparent pixels.

| Canonical ID | Display name | Expected filename | Currently resolved | Dimensions | PNG colour | Alpha | Bytes | Code | Duplicate/typo candidate in read-only main audit | Missing dedicated PNG | Fallback used |
|---|---|---|---|---|---|---|---:|---|---|---|---|
| 3DO | 3DO Interactive Multiplayer | `3do.png` | console glyph | - | - | - | - | key only | `3DO Interactive Multiplayer.png` (noncanonical name) | yes | `console.svg` |
| Acorn Archimedes | Acorn Archimedes | `acornarchimedes.png` | `acornarchimedes.png` | 1024x1024 | RGBA8 | yes | 1,662,887 | bundled | `Archimedes.png` | no | no |
| Acorn Electron | Acorn Electron | `acornelectron.png` | computer glyph | - | - | - | - | key only | `electron.png` | yes | `computer.svg` |
| AmigaCD32 | Amiga CD32 | `amigacd32.png` | optical-disc glyph | - | - | - | - | key only | same-name RGB candidate | yes | `optical-disc.svg` |
| Amstrad CPC | Amstrad CPC | `amstradcpc.png` | computer glyph | - | - | - | - | key only | same-name RGB candidate | yes | `computer.svg` |
| Apple II | Apple II | `appleii.png` | computer glyph | - | - | - | - | key only | `apple2.png` | yes | `computer.svg` |
| Macintosh | Macintosh | `macintosh.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| Arcade | Arcade | `arcade.png` | arcade glyph | - | - | - | - | key only | same-name RGB candidate | yes | `arcade.svg` |
| Atari2600 | Atari 2600 | `atari2600.png` | cartridge glyph | - | - | - | - | key only | `atrii2600.png` (typo) | yes | `cartridge.svg` |
| Atari5200 | Atari 5200 | `atari5200.png` | cartridge glyph | - | - | - | - | key only | `atria5200.png` (typo) | yes | `cartridge.svg` |
| Atari7800 | Atari 7800 | `atari7800.png` | cartridge glyph | - | - | - | - | key only | `atrai7800.png` (typo) | yes | `cartridge.svg` |
| Atari 8-bit | Atari 8-bit | `atari8bit.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| Atari Jaguar | Atari Jaguar | `atarijaguar.png` | cartridge glyph | - | - | - | - | key only | same-name RGB candidate | yes | `cartridge.svg` |
| Atari Lynx | Atari Lynx | `atarilynx.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| AtariST | Atari ST | `atarist.png` | computer glyph | - | - | - | - | key only | `Atari ST.png` | yes | `computer.svg` |
| WonderSwan | WonderSwan | `wonderswan.png` | handheld glyph | - | - | - | - | key only | none | yes | `handheld.svg` |
| WonderSwan Color | WonderSwan Color | `wonderswancolor.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| BBC Micro | BBC Micro | `bbcmicro.png` | computer glyph | - | - | - | - | key only | `bbcmodelb.png` | yes | `computer.svg` |
| ColecoVision | ColecoVision | `colecovision.png` | console glyph | - | - | - | - | key only | `ColecoVision.png` (case) | yes | `console.svg` |
| Commodore 128 | Commodore 128 | `commodore128.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| Commodore 64 | Commodore 64 | `commodore64.png` | computer glyph | - | - | - | - | key only | `commidore64.png` (typo) | yes | `computer.svg` |
| Amiga | Commodore Amiga | `amiga.png` | `amiga.png` | 1024x1024 | RGBA8 | yes | 1,635,956 | bundled | UUID-named Amiga-like candidate is unverified | no | no |
| Commodore CDTV | Commodore CDTV | `commodorecdtv.png` | optical-disc glyph | - | - | - | - | key only | none | yes | `optical-disc.svg` |
| VIC-20 | Commodore VIC-20 | `vic20.png` | computer glyph | - | - | - | - | key only | same-name RGB candidate | yes | `computer.svg` |
| FM Towns | FM Towns | `fmtowns.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| Vectrex | GCE Vectrex | `vectrex.png` | console glyph | - | - | - | - | key only | none | yes | `console.svg` |
| Intellivision | Intellivision | `intellivision.png` | console glyph | - | - | - | - | key only | none | yes | `console.svg` |
| Xbox | Microsoft Xbox | `xbox.png` | `xbox.png` | 1024x1024 | RGBA8 | yes | 1,582,929 | bundled | same-name RGB replacement candidate | no | no |
| Xbox360 | Microsoft Xbox 360 | `xbox360.png` | `xbox360.png` | 1024x1024 | RGBA8 | yes | 1,533,459 | bundled | same-name RGB replacement candidate | no | no |
| DOS | MS-DOS | `dos.png` | computer glyph | - | - | - | - | key only | `pcbox.png` is not authoritative | yes | `computer.svg` |
| MSX | MSX | `msx.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| MSX2 | MSX2 | `msx2.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| NEC PC-8801 | NEC PC-8801 | `necpc8801.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| PC-98 | NEC PC-98 | `pc98.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| NEC PC-9801 | NEC PC-9801 | `necpc9801.png` | computer glyph | - | - | - | - | key only | none | yes | `computer.svg` |
| NeoGeo | Neo Geo | `neogeo.png` | console glyph | - | - | - | - | key only | same-name RGB candidate | yes | `console.svg` |
| NeoGeo64 | Neo Geo 64 | `neogeo64.png` | console glyph | - | - | - | - | key only | none | yes | `console.svg` |
| Neo Geo CD | Neo Geo CD | `neogeocd.png` | optical-disc glyph | - | - | - | - | key only | none | yes | `optical-disc.svg` |
| Neo Geo Pocket | Neo Geo Pocket | `neogeopocket.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| Neo Geo Pocket Color | Neo Geo Pocket Color | `neogeopocketcolor.png` | handheld glyph | - | - | - | - | key only | `neogeocolor.png` | yes | `handheld.svg` |
| Nintendo 3DS | Nintendo 3DS | `nintendo3ds.png` | handheld glyph | - | - | - | - | key only | `3ds.png` | yes | `handheld.svg` |
| N64 | Nintendo 64 | `n64.png` | `n64.png` | 1024x1024 | RGBA8 | yes | 1,571,533 | bundled | same-name RGB replacement candidate | no | no |
| Nintendo DS | Nintendo DS | `nintendods.png` | handheld glyph | - | - | - | - | key only | none | yes | `handheld.svg` |
| NES | Nintendo Entertainment System | `nes.png` | console glyph | - | - | - | - | key only | same-name RGB candidate | yes | `console.svg` |
| Game Boy | Nintendo Game Boy | `gameboy.png` | `gameboy.png` | 1024x1024 | RGBA8 | yes | 1,576,726 | bundled | same-name RGB replacement candidate | no | no |
| Game Boy Advance | Nintendo Game Boy Advance | `gameboyadvance.png` | handheld glyph | - | - | - | - | key only | `gba.png` | yes | `handheld.svg` |
| Game Boy Color | Nintendo Game Boy Color | `gameboycolor.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| GameCube | Nintendo GameCube | `gamecube.png` | `gamecube.png` | 1024x1024 | RGBA8 | yes | 1,594,670 | bundled | same-name RGB replacement candidate | no | no |
| Switch | Nintendo Switch | `switch.png` | `switch.png` | 1024x1024 | RGBA8 | yes | 1,509,672 | bundled | same-name RGB replacement candidate | no | no |
| Virtual Boy | Nintendo Virtual Boy | `virtualboy.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| Wii | Nintendo Wii | `wii.png` | `wii.png` | 1024x1024 | RGBA8 | yes | 1,489,732 | bundled | same-name RGB replacement candidate | no | no |
| WiiU | Nintendo Wii U | `wiiu.png` | `wiiu.png` | 1024x1024 | RGBA8 | yes | 1,641,337 | bundled | same-name RGB replacement candidate | no | no |
| NGage | Nokia N-Gage | `ngage.png` | handheld glyph | - | - | - | - | key only | none | yes | `handheld.svg` |
| PC | PC | `pc.png` | computer glyph | - | - | - | - | key only | `pcbox.png` is ambiguous | yes | `computer.svg` |
| PC Engine | PC Engine | `pcengine.png` | cartridge glyph | - | - | - | - | key only | none | yes | `cartridge.svg` |
| PC Engine CD | PC Engine CD | `pcenginecd.png` | optical-disc glyph | - | - | - | - | key only | `turbogratxcd.png` is a regional-name/typo candidate | yes | `optical-disc.svg` |
| Philips CD-i | Philips CD-i | `philipscdi.png` | optical-disc glyph | - | - | - | - | key only | `philpscdi.png` (typo) | yes | `optical-disc.svg` |
| ScummVM | ScummVM | `scummvm.png` | computer glyph | - | - | - | - | key only | `scumvm.png` (typo) | yes | `computer.svg` |
| Sega 32X | Sega 32X | `sega32x.png` | console glyph | - | - | - | - | key only | same-name RGB candidate | yes | `console.svg` |
| Dreamcast | Sega Dreamcast | `dreamcast.png` | `dreamcast.png` | 1024x1024 | RGBA8 | yes | 1,597,369 | bundled | `dreamcatst.png` and same-name replacement | no | no |
| GameGear | Sega Game Gear | `gamegear.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| MasterSystem | Sega Master System | `mastersystem.png` | console glyph | - | - | - | - | key only | none | yes | `console.svg` |
| MegaDrive | Sega Mega Drive / Genesis | `megadrive.png` | `megadrive.png` | 1024x1024 | RGBA8 | yes | 1,555,855 | bundled | same-name RGB replacement candidate | no | no |
| Sega CD | Sega/Mega CD | `segacd.png` | optical-disc glyph | - | - | - | - | key only | none | yes | `optical-disc.svg` |
| Saturn | Sega Saturn | `saturn.png` | `saturn.png` | 1024x1024 | RGBA8 | yes | 1,537,579 | bundled | same-name RGB replacement candidate | no | no |
| Sharp X68000 | Sharp X68000 | `sharpx68000.png` | computer glyph | - | - | - | - | key only | same-name RGB candidate | yes | `computer.svg` |
| PSX | Sony PlayStation | `psx.png` | `psx.png` | 1024x1024 | RGBA8 | yes | 1,575,183 | bundled | legacy `playstation.png` renamed; separate untracked `psx.png` candidate | no | no |
| PS2 | Sony PlayStation 2 | `ps2.png` | `ps2.png` | 1024x1024 | RGBA8 | yes | 1,483,067 | bundled | legacy `playstation2.png` renamed; untracked `psx2.png` candidate | no | no |
| PS3 | Sony PlayStation 3 | `ps3.png` | `ps3.png` | 1024x1024 | RGBA8 | yes | 1,520,358 | bundled | legacy `playstation3.png` renamed; untracked `psx3.png` candidate | no | no |
| PSP | Sony PSP | `psp.png` | handheld glyph | - | - | - | - | key only | same-name RGB candidate | yes | `handheld.svg` |
| PlayStation Vita | Sony PlayStation Vita | `playstationvita.png` | handheld glyph | - | - | - | - | key only | `vita.png` | yes | `handheld.svg` |
| SNES | Super Nintendo Entertainment System | `snes.png` | `snes.png` | 1024x1024 | RGBA8 | yes | 1,633,370 | bundled | same-name RGB replacement candidate | no | no |
| TurboGrafx-16 | TurboGrafx-16 | `turbografx16.png` | cartridge glyph | - | - | - | - | key only | `turbogratx16.png` (typo) | yes | `cartridge.svg` |
| ZX Spectrum | ZX Spectrum | `zxspectrum.png` | computer glyph | - | - | - | - | key only | `spectrum128k.png` is model-specific | yes | `computer.svg` |

Totals: **74 canonical platforms**, **17 dedicated bundled PNGs**, and **57
intentional generic fallbacks / missing dedicated PNGs**.

## Read-only audit of uncommitted candidate files in `main`

These files were inspected without copying or modifying them. Every new RGB
candidate is 1024x1024, non-interlaced, non-animated, and has no PNG alpha or
`tRNS` transparency. They are not committed here because their provenance was
not explicitly confirmed and many do not meet the transparent-background goal.
The directory contained 67 PNGs: 17 tracked canonical paths, 12 of those paths
modified in place, and 50 additional untracked candidates. SHA-256 comparison
found no byte-identical duplicate files; “duplicate candidate” below means two
different renders competing for one canonical platform.

The 12 tracked paths modified in `main` (`dreamcast.png`, `gameboy.png`,
`gamecube.png`, `megadrive.png`, `n64.png`, `saturn.png`, `snes.png`,
`switch.png`, `wii.png`, `wiiu.png`, `xbox.png`, `xbox360.png`) are RGB
replacement candidates; this branch retains the reviewed RGBA originals.

Every row below is 1024x1024, 8-bit and non-interlaced. `RGB/no` means no
alpha channel and no `tRNS` chunk; `RGBA/yes` records an alpha channel. File
sizes describe the read-only files in `main`, not files committed by this
branch.

| File | Main state | Bytes | Colour/alpha | Canonical disposition |
|---|---|---:|---|---|
| `dreamcast.png` | modified tracked | 757,545 | RGB/no | replacement for existing `dreamcast.png`; not imported |
| `gameboy.png` | modified tracked | 875,124 | RGB/no | replacement for existing `gameboy.png`; not imported |
| `gamecube.png` | modified tracked | 842,028 | RGB/no | replacement for existing `gamecube.png`; not imported |
| `megadrive.png` | modified tracked | 802,388 | RGB/no | replacement for existing `megadrive.png`; not imported |
| `n64.png` | modified tracked | 802,347 | RGB/no | replacement for existing `n64.png`; not imported |
| `saturn.png` | modified tracked | 927,140 | RGB/no | replacement for existing `saturn.png`; not imported |
| `snes.png` | modified tracked | 881,030 | RGB/no | replacement for existing `snes.png`; not imported |
| `switch.png` | modified tracked | 696,768 | RGB/no | replacement for existing `switch.png`; not imported |
| `wii.png` | modified tracked | 726,350 | RGB/no | replacement for existing `wii.png`; not imported |
| `wiiu.png` | modified tracked | 697,270 | RGB/no | replacement for existing `wiiu.png`; not imported |
| `xbox.png` | modified tracked | 752,500 | RGB/no | replacement for existing `xbox.png`; not imported |
| `xbox360.png` | modified tracked | 761,771 | RGB/no | replacement for existing `xbox360.png`; not imported |
| `3DO Interactive Multiplayer.png` | untracked | 890,613 | RGB/no | rename candidate for `3do.png`; approval pending |
| `3ds.png` | untracked | 976,003 | RGB/no | rename candidate for `nintendo3ds.png`; approval pending |
| `4448b039-69a6-4690-a61f-dfc5393c3069.png` | untracked | 878,502 | RGB/no | unmapped; UUID forbidden as preferred name |
| `Archimedes.png` | untracked | 1,002,504 | RGB/no | duplicate candidate for `acornarchimedes.png` |
| `Atari ST.png` | untracked | 840,514 | RGB/no | rename candidate for `atarist.png`; approval pending |
| `ColecoVision.png` | untracked | 939,288 | RGB/no | case-normalisation candidate for `colecovision.png` |
| `amigacd32.png` | untracked | 794,226 | RGB/no | canonical name; approval pending |
| `amstradcpc.png` | untracked | 772,677 | RGB/no | canonical name; approval pending |
| `apple2.png` | untracked | 920,734 | RGB/no | rename candidate for `appleii.png`; approval pending |
| `arcade.png` | untracked | 818,199 | RGB/no | canonical name; approval pending |
| `atarijaguar.png` | untracked | 901,036 | RGB/no | canonical name; approval pending |
| `atarilynx.png` | untracked | 780,314 | RGB/no | canonical name; approval pending |
| `atrai7800.png` | untracked | 848,450 | RGB/no | typo; candidate for `atari7800.png` |
| `atria5200.png` | untracked | 853,688 | RGB/no | typo; candidate for `atari5200.png` |
| `atrii2600.png` | untracked | 984,116 | RGB/no | typo; candidate for `atari2600.png` |
| `bbcmodelb.png` | untracked | 904,136 | RGB/no | candidate for `bbcmicro.png`; model choice needs approval |
| `commidore64.png` | untracked | 844,206 | RGB/no | typo; candidate for `commodore64.png` |
| `dragon.png` | untracked | 1,270,129 | RGB/no | no canonical platform target |
| `dreamcatst.png` | untracked | 1,601,393 | RGBA/yes | misspelled duplicate candidate for `dreamcast.png` |
| `electron.png` | untracked | 870,857 | RGB/no | candidate for `acornelectron.png`; approval pending |
| `gameboycolor.png` | untracked | 878,521 | RGB/no | canonical name; approval pending |
| `gamegear.png` | untracked | 867,674 | RGB/no | canonical name; approval pending |
| `gba.png` | untracked | 814,705 | RGB/no | candidate for `gameboyadvance.png`; approval pending |
| `neogeo.png` | untracked | 803,165 | RGB/no | canonical name; approval pending |
| `neogeocolor.png` | untracked | 825,968 | RGB/no | candidate for `neogeopocketcolor.png`; identity needs confirmation |
| `neogeopocket.png` | untracked | 949,495 | RGB/no | canonical name; approval pending |
| `nes.png` | untracked | 769,903 | RGB/no | canonical name; approval pending |
| `pcbox.png` | untracked | 883,703 | RGB/no | ambiguous between `pc.png`, `dos.png`, and other computers |
| `pcfx.png` | untracked | 829,964 | RGB/no | no canonical platform target |
| `philpscdi.png` | untracked | 764,198 | RGB/no | typo; candidate for `philipscdi.png` |
| `psp.png` | untracked | 719,556 | RGB/no | canonical name; approval pending |
| `psx.png` | untracked | 889,127 | RGB/no | duplicate candidate for canonical `psx.png` |
| `psx2.png` | untracked | 837,234 | RGB/no | noncanonical candidate for `ps2.png` |
| `psx3.png` | untracked | 818,227 | RGB/no | noncanonical candidate for `ps3.png` |
| `psx4.png` | untracked | 729,029 | RGB/no | no canonical platform target |
| `psx5.png` | untracked | 635,933 | RGB/no | no canonical platform target |
| `scumvm.png` | untracked | 1,114,227 | RGB/no | typo; candidate for `scummvm.png` |
| `sega32x.png` | untracked | 874,323 | RGB/no | canonical name; approval pending |
| `sharpx68000.png` | untracked | 952,632 | RGB/no | canonical name; approval pending |
| `spectrum128k.png` | untracked | 902,197 | RGB/no | model-specific candidate for `zxspectrum.png` |
| `supergratx.png` | untracked | 986,817 | RGB/no | typo and no canonical SuperGrafx target |
| `tandycomputer.png` | untracked | 1,075,839 | RGB/no | no canonical platform target |
| `turbogratx16.png` | untracked | 854,602 | RGB/no | typo; candidate for `turbografx16.png` |
| `turbogratxcd.png` | untracked | 852,468 | RGB/no | regional-name candidate for `pcenginecd.png`; identity needs confirmation |
| `vc4000.png` | untracked | 884,169 | RGB/no | no canonical platform target |
| `vic20.png` | untracked | 795,336 | RGB/no | canonical name; approval pending |
| `virtualboy.png` | untracked | 725,194 | RGB/no | canonical name; approval pending |
| `vita.png` | untracked | 804,056 | RGB/no | candidate for `playstationvita.png`; approval pending |
| `wonderswancolor.png` | untracked | 821,909 | RGB/no | canonical name; approval pending |
| `xboxone.png` | untracked | 739,918 | RGB/no | no canonical platform target |

| Candidate files | Audit result |
|---|---|
| `3DO Interactive Multiplayer.png`, `Archimedes.png`, `Atari ST.png`, `ColecoVision.png` | Recognisable naming intent, but spaces/case violate the canonical convention. |
| `atrii2600.png`, `atria5200.png`, `atrai7800.png`, `commidore64.png`, `philpscdi.png`, `scumvm.png`, `turbogratx16.png`, `turbogratxcd.png`, `supergratx.png`, `dreamcatst.png` | Misspelled names; some also target no canonical platform. |
| `playstation.png`/`psx.png`, `playstation2.png`/`psx2.png`, `playstation3.png`/`psx3.png` | Legacy tracked names plus separate alternatives for three canonical machines; canonical names are `psx.png`, `ps2.png`, and `ps3.png`. |
| `psx4.png`, `psx5.png`, `xboxone.png`, `pcfx.png`, `supergratx.png`, `dragon.png`, `tandycomputer.png`, `vc4000.png` | Depict/name platforms absent from the current canonical registry; retained only in the untouched source worktree. |
| `4448b039-69a6-4690-a61f-dfc5393c3069.png` | No code, filename, or embedded subject metadata establishes a platform. Metadata only says AI-generated; visual inspection suggests an Amiga-style keyboard computer, which is insufficient to map it. |
| `pcbox.png` | Generic PC-like naming cannot distinguish `PC`, `DOS`, or a particular computer platform. |
| `neogeocolor.png` | Likely naming intent overlaps Neo Geo Pocket Color, but the filename is not canonical. |
| `spectrum128k.png` | A model-specific ZX Spectrum candidate, not a canonical filename. |
| `3ds.png`, `amigacd32.png`, `amstradcpc.png`, `apple2.png`, `arcade.png`, `atarijaguar.png`, `atarilynx.png`, `bbcmodelb.png`, `electron.png`, `gameboycolor.png`, `gamegear.png`, `gba.png`, `neogeo.png`, `neogeopocket.png`, `nes.png`, `psp.png`, `sega32x.png`, `sharpx68000.png`, `vic20.png`, `virtualboy.png`, `vita.png`, `wonderswancolor.png` | Plausible one-platform candidates, but either the filename differs from the canonical convention or provenance/transparency approval is pending. |

No candidate was deleted or renamed. No identity was assigned solely from its
appearance.

## SVG and fallback inventory

The category SVGs `console.svg`, `handheld.svg`, `computer.svg`, `arcade.svg`,
`optical-disc.svg`, `cartridge.svg`, and `unknown.svg` remain the documented
source/licensing records for the native fallback glyphs. The GUI does not parse
SVG at runtime. `gamecube.svg`, `playstation2.svg`, and `xbox.svg` are retained
as documented legacy exact-platform references; their PNGs are preferred and
they are not embedded a second time.

## Ready-to-copy prompts for missing dedicated artwork

Save each accepted result under the filename shown. These prompts intentionally
exclude the 17 platforms with reviewed bundled PNGs.

- `3do.png`: `Create a 1024x1024 square platform icon depicting the 3DO Interactive Multiplayer. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `acornelectron.png`: `Create a 1024x1024 square platform icon depicting the Acorn Electron. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `amigacd32.png`: `Create a 1024x1024 square platform icon depicting the Amiga CD32. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `amstradcpc.png`: `Create a 1024x1024 square platform icon depicting the Amstrad CPC. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `appleii.png`: `Create a 1024x1024 square platform icon depicting the Apple II. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `macintosh.png`: `Create a 1024x1024 square platform icon depicting the Macintosh. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard and mouse visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `arcade.png`: `Create a 1024x1024 square platform icon depicting an Arcade cabinet. Transparent background. Machine centred in a three-quarter front product view. Entire cabinet and controls visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atari2600.png`: `Create a 1024x1024 square platform icon depicting the Atari 2600. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atari5200.png`: `Create a 1024x1024 square platform icon depicting the Atari 5200. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atari7800.png`: `Create a 1024x1024 square platform icon depicting the Atari 7800. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atari8bit.png`: `Create a 1024x1024 square platform icon depicting the Atari 8-bit computer family. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atarijaguar.png`: `Create a 1024x1024 square platform icon depicting the Atari Jaguar. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atarilynx.png`: `Create a 1024x1024 square platform icon depicting the Atari Lynx. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `atarist.png`: `Create a 1024x1024 square platform icon depicting the Atari ST. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `wonderswan.png`: `Create a 1024x1024 square platform icon depicting the WonderSwan. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `wonderswancolor.png`: `Create a 1024x1024 square platform icon depicting the WonderSwan Color. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `bbcmicro.png`: `Create a 1024x1024 square platform icon depicting the BBC Micro. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `colecovision.png`: `Create a 1024x1024 square platform icon depicting the ColecoVision. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `commodore128.png`: `Create a 1024x1024 square platform icon depicting the Commodore 128. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `commodore64.png`: `Create a 1024x1024 square platform icon depicting the Commodore 64. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `commodorecdtv.png`: `Create a 1024x1024 square platform icon depicting the Commodore CDTV. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `vic20.png`: `Create a 1024x1024 square platform icon depicting the Commodore VIC-20. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `fmtowns.png`: `Create a 1024x1024 square platform icon depicting the FM Towns. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `vectrex.png`: `Create a 1024x1024 square platform icon depicting the GCE Vectrex. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `intellivision.png`: `Create a 1024x1024 square platform icon depicting the Intellivision. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `dos.png`: `Create a 1024x1024 square platform icon depicting a representative MS-DOS PC. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard and mouse visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `msx.png`: `Create a 1024x1024 square platform icon depicting an MSX computer. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `msx2.png`: `Create a 1024x1024 square platform icon depicting an MSX2 computer. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `necpc8801.png`: `Create a 1024x1024 square platform icon depicting the NEC PC-8801. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `pc98.png`: `Create a 1024x1024 square platform icon depicting the NEC PC-98 family. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `necpc9801.png`: `Create a 1024x1024 square platform icon depicting the NEC PC-9801. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `neogeo.png`: `Create a 1024x1024 square platform icon depicting the Neo Geo. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `neogeo64.png`: `Create a 1024x1024 square platform icon depicting the Neo Geo 64. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `neogeocd.png`: `Create a 1024x1024 square platform icon depicting the Neo Geo CD. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `neogeopocket.png`: `Create a 1024x1024 square platform icon depicting the Neo Geo Pocket. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `neogeopocketcolor.png`: `Create a 1024x1024 square platform icon depicting the Neo Geo Pocket Color. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `nintendo3ds.png`: `Create a 1024x1024 square platform icon depicting the Nintendo 3DS. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `nintendods.png`: `Create a 1024x1024 square platform icon depicting the Nintendo DS. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `nes.png`: `Create a 1024x1024 square platform icon depicting the Nintendo Entertainment System. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `gameboyadvance.png`: `Create a 1024x1024 square platform icon depicting the Nintendo Game Boy Advance. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `gameboycolor.png`: `Create a 1024x1024 square platform icon depicting the Nintendo Game Boy Color. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `virtualboy.png`: `Create a 1024x1024 square platform icon depicting the Nintendo Virtual Boy. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `ngage.png`: `Create a 1024x1024 square platform icon depicting the Nokia N-Gage. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `pc.png`: `Create a 1024x1024 square platform icon depicting a representative PC. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard and mouse visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `pcengine.png`: `Create a 1024x1024 square platform icon depicting the PC Engine. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `pcenginecd.png`: `Create a 1024x1024 square platform icon depicting the PC Engine CD. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `philipscdi.png`: `Create a 1024x1024 square platform icon depicting the Philips CD-i. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `scummvm.png`: `Create a 1024x1024 square platform icon depicting a neutral computer representing ScummVM. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard and mouse visible. Comfortable padding around every edge. No game characters, text, logos added as separate typography, people, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `sega32x.png`: `Create a 1024x1024 square platform icon depicting the Sega 32X hardware. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `gamegear.png`: `Create a 1024x1024 square platform icon depicting the Sega Game Gear. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `mastersystem.png`: `Create a 1024x1024 square platform icon depicting the Sega Master System. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `segacd.png`: `Create a 1024x1024 square platform icon depicting the Sega CD / Mega-CD hardware. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `sharpx68000.png`: `Create a 1024x1024 square platform icon depicting the Sharp X68000. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `psp.png`: `Create a 1024x1024 square platform icon depicting the Sony PSP. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `playstationvita.png`: `Create a 1024x1024 square platform icon depicting the Sony PlayStation Vita. Transparent background. Machine centred in a three-quarter front product view. Entire handheld visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `turbografx16.png`: `Create a 1024x1024 square platform icon depicting the TurboGrafx-16. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential controller visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
- `zxspectrum.png`: `Create a 1024x1024 square platform icon depicting the ZX Spectrum. Transparent background. Machine centred in a three-quarter front product view. Entire machine and essential keyboard visible. Comfortable padding around every edge. No text, logos added as separate typography, people, characters, scenery or border. Clean realistic product-render appearance. Consistent camera angle, scale and lighting with the ArchiveFS platform artwork set.`
