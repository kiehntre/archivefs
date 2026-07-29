# Platform artwork

Status: Platform Artwork Pack v1 adds exact hardware illustrations to the
existing Gamer View platform shelf without changing its filtering, selection,
scrolling, or fallback model.

## Runtime policy

Artwork resolution is deterministic and strictly ordered:

1. A valid PNG in the explicitly configured custom-artwork directory, named
   for the resolved exact asset id (or fallback category id).
2. The exact Platform Artwork Pack v1 PNG compiled into the GUI executable.
3. The existing category fallback native glyph: console, handheld, computer,
   arcade, optical-disc, or cartridge.
4. The existing Unknown native glyph.

Compact game-list rows use a related three-step chain: a valid custom
`game-<normalised-title>.png`, the resolved platform artwork above, then the
Unknown glyph. Game and platform textures share the bounded session cache, so
scrolling does not reopen or decode files each frame.

Exact aliases are tested before category inference. Matching is
case-insensitive and normalization removes spaces and hyphens only. There is
no substring, fuzzy, or broad family matching: Wii U cannot become Wii, and
Xbox 360 cannot become original Xbox.

Every bundled PNG is compiled with `include_bytes!` from a closed static
registry in `crates/archivefs-gui/src/main.rs`. Installed builds never read a
source-tree asset path. PNGs are decoded in process using the existing
`image` dependency with PNG support only, uploaded as egui textures, and
cached for the GUI session. A malformed bundled PNG is cached as a failure
and falls through to the category/Unknown glyph. Images are aspect-fitted,
centred, and never cropped or stretched.

There is no runtime artwork network access, URL lookup, update check,
download, external image command, or automatic launcher/theme discovery.
Release builds are fully offline with respect to artwork and contain the
same compile-time byte registry for identical inputs.

## Platform Artwork Pack v1 aliases

| Bundled PNG | Exact aliases |
|---|---|
| `acornarchimedes.png` | Acorn Archimedes; Archimedes |
| `amiga.png` | Amiga; Commodore Amiga |
| `dreamcast.png` | Dreamcast; Sega Dreamcast |
| `gameboy.png` | Game Boy; Nintendo Game Boy |
| `gamecube.png` | GameCube; Nintendo GameCube; Nintendo Game Cube |
| `megadrive.png` | Mega Drive; Sega Mega Drive; Genesis; Sega Genesis |
| `n64.png` | Nintendo 64; N64 |
| `playstation.png` | PlayStation; PlayStation 1; PS1; PSX |
| `playstation2.png` | PlayStation 2; PS2 |
| `playstation3.png` | PlayStation 3; PS3 |
| `saturn.png` | Saturn; Sega Saturn |
| `snes.png` | Super Nintendo; Super Nintendo Entertainment System; SNES; Super Famicom |
| `switch.png` | Nintendo Switch; Switch |
| `wii.png` | Nintendo Wii; Wii |
| `wiiu.png` | Nintendo Wii U; Wii U; WiiU |
| `xbox.png` | Xbox; Original Xbox |
| `xbox360.png` | Xbox 360; X360 |

Aliases determine both the bundled image and the filename for a higher
priority custom override. For example, `PSX` resolves to
`playstation.png`, not `psx.png`.

## Inspection and PNG manifest

All files were inspected at native size and on the current dark background
at 96×96. Encoded format for every row is 1024×1024, 8-bit RGBA,
non-interlaced PNG. Every file contains a real alpha channel with both fully
transparent and fully opaque pixels. Padding is the transparent distance in
pixels from the alpha-content bounds to left/top/right/bottom canvas edges.

| Filename | Encoded bytes | Transparent padding L/T/R/B | Text/logo inspection | 96×96 and suitability |
|---|---:|---:|---|---|
| `acornarchimedes.png` | 1,662,887 | 60/102/48/58 | No visible text or logo | Readable; complete computer/mouse; accepted |
| `amiga.png` | 1,635,956 | 60/200/3/89 | No visible text or logo | Readable; right-heavy but not cropped; accepted |
| `dreamcast.png` | 1,597,369 | 20/6/3/33 | No visible text or logo | Readable; complete console/controller; accepted |
| `gameboy.png` | 1,576,726 | 256/102/3/49 | No visible text or logo | Readable; asymmetric horizontal padding; accepted |
| `gamecube.png` | 1,594,670 | 172/142/131/89 | No visible text or logo | Readable and well centred; accepted |
| `megadrive.png` | 1,555,855 | 85/6/3/33 | No visible text or logo | Readable; complete console; accepted |
| `n64.png` | 1,571,533 | 44/6/3/65 | No visible text or logo | Readable; complete console/controller; accepted |
| `playstation.png` | 1,575,183 | 111/102/3/65 | No visible text or logo | Readable; right-heavy but complete; accepted |
| `playstation2.png` | 1,483,067 | 108/93/3/89 | No visible text or logo | Readable; slim tower remains distinct; accepted |
| `playstation3.png` | 1,520,358 | 92/142/3/89 | No visible text or logo | Readable; complete console/controller; accepted |
| `saturn.png` | 1,537,579 | 184/142/80/58 | No visible text or logo | Readable and complete; accepted |
| `snes.png` | 1,633,370 | 140/102/59/58 | No visible text or logo | Readable and complete; accepted |
| `switch.png` | 1,509,672 | 92/142/19/89 | No visible text or logo | Readable and complete; accepted |
| `wii.png` | 1,489,732 | 4/14/3/9 | No visible text or logo | Readable; subject nearly fills canvas but is not cropped; accepted |
| `wiiu.png` | 1,641,337 | 96/176/56/58 | No visible text or logo | Readable and complete; accepted |
| `xbox.png` | 1,582,929 | 44/6/3/33 | No text; a green X-like hardware detail is visible | Readable and complete; accepted; no wordmark detected |
| `xbox360.png` | 1,533,459 | 276/62/3/49 | No visible text or wordmark | Readable; asymmetric horizontal padding; accepted |

Total bundled PNG size: **26,701,682 bytes (25.46 MiB)**. No supplied v1
asset was rejected. `dreamcatst.png` is not part of the pack or registry.
No image was regenerated, resized on disk, recompressed, or otherwise
altered during integration.

## Provenance and endorsement

The PNGs were supplied for this milestone as original/generated ArchiveFS
project artwork. The repository does not record a third-party source,
photographer, or external artwork pack for them. No manufacturer wordmarks,
manufacturer logos, or copied photographs were intentionally used. The
inspection above records the one visible X-like hardware detail rather than
silently treating it as absent. These illustrations necessarily identify
recognisable hardware but do not claim trademark ownership, official status,
manufacturer approval, or endorsement. They are distributed under the same
licence terms as the repository's own source unless a future provenance
record says otherwise.

## Custom artwork directory

Advanced View → Settings → “5. Platform artwork” accepts an optional local
directory. Custom PNGs remain higher priority than the bundled pack and
retain the established safety limits:

- regular files directly inside the configured directory only; symlinks,
  invalid ids, and non-files are rejected;
- PNG only; custom SVG is never parsed;
- maximum encoded size 1 MiB;
- maximum dimensions 1024×1024 and maximum decoded allocation 4 MiB;
- malformed, oversized, missing, or unsupported files fall through safely;
- successful textures and failed decode fingerprints are cached by directory,
  asset id, length, and modification time;
- custom files are read in place and never copied or modified.

The directory preference is persisted in
`~/.config/archivefs/platform_artwork_directory.txt` only when explicitly
changed. Category custom filenames (`console.png`, `handheld.png`, etc.)
continue to work for platforms without an exact bundled mapping.

## SVG/category fallbacks

The existing SVG files remain canonical category/fallback references,
lightweight source records, and licensing documentation. They are not deleted
or parsed at runtime; egui draws their established native equivalents.

| SVG | Runtime role |
|---|---|
| `console.svg` | Console category fallback |
| `handheld.svg` | Handheld category fallback |
| `computer.svg` | Computer category fallback |
| `arcade.svg` | Arcade category fallback |
| `optical-disc.svg` | Optical-disc category fallback |
| `cartridge.svg` | Cartridge category fallback |
| `unknown.svg` | Final Unknown fallback |
| `gamecube.svg` | Canonical legacy abstract GameCube reference |
| `playstation2.svg` | Canonical legacy abstract PS2 reference |
| `xbox.svg` | Canonical legacy abstract Xbox reference |

Fallback category examples remain unchanged: Game Boy Advance uses handheld,
NES uses console, PC uses computer, Arcade uses arcade, Amiga CD32 uses
optical-disc, Atari 2600 uses cartridge, and an unrecognised platform uses
Unknown.

## Presentation and testing

The shelf remains one horizontally scrolling row. Cards retain responsive
96–124 logical-pixel widths, platform name, count, selected state, standard
button keyboard focus/activation, full-name tooltip/accessibility label, and
1024×600 behavior. Hardware illustrations are aspect-fitted at 60 logical
pixels in an 88-pixel card, centred with transparent padding preserved. Compact
game rows use a recognisable 38-pixel thumbnail in a 46-pixel row.

Tests cover the complete registry and alias table, case normalization,
related-platform separation, category and Unknown fallbacks, custom override
precedence, malformed custom and bundled data, deterministic embedded decode,
absence of a bundled filesystem input, aspect fitting, card sizing, and
unchanged platform filtering/selection state. Pixel-perfect screenshots are
deliberately not used.
