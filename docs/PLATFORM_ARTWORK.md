# Platform artwork

Status: Platform Artwork Pack v1 adds exact hardware illustrations to the
existing Gamer View platform shelf without changing its filtering, selection,
scrolling, or fallback model.

The complete 74-platform inventory, candidate-file audit, canonical filenames,
and prompts for missing images live in
[`PLATFORM_ARTWORK_STATUS.md`](PLATFORM_ARTWORK_STATUS.md).

## Runtime policy

Artwork resolution is deterministic and strictly ordered:

1. A valid ArchiveFS-managed custom PNG named for the canonical platform ID.
2. The exact Platform Artwork Pack v1 PNG compiled into the GUI executable.
3. A valid managed custom category PNG, when present.
4. The existing category fallback native glyph: console, handheld, computer,
   arcade, optical-disc, or cartridge.
5. The existing Unknown native glyph.

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
| `psx.png` | PlayStation; PlayStation 1; PS1; PSX |
| `ps2.png` | PlayStation 2; PS2 |
| `ps3.png` | PlayStation 3; PS3 |
| `saturn.png` | Saturn; Sega Saturn |
| `snes.png` | Super Nintendo; Super Nintendo Entertainment System; SNES; Super Famicom |
| `switch.png` | Nintendo Switch; Switch |
| `wii.png` | Nintendo Wii; Wii |
| `wiiu.png` | Nintendo Wii U; Wii U; WiiU |
| `xbox.png` | Xbox; Original Xbox |
| `xbox360.png` | Xbox 360; X360 |

Aliases first resolve through the canonical platform registry. The persisted
canonical ID then determines the bundled image and higher-priority custom
override filename. For example, PlayStation aliases resolve to canonical
`PSX`, then to `psx.png`; no display-name guessing is involved.

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
| `psx.png` | 1,575,183 | 111/102/3/65 | No visible text or logo | Readable; right-heavy but complete; accepted |
| `ps2.png` | 1,483,067 | 108/93/3/89 | No visible text or logo | Readable; slim tower remains distinct; accepted |
| `ps3.png` | 1,520,358 | 92/142/3/89 | No visible text or logo | Readable; complete console/controller; accepted |
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

## User-managed artwork

Advanced View → Settings → “5. Platform artwork” manages upgrade-stable user
artwork under `~/.local/share/archivefs/platform-artwork/`. ArchiveFS never
writes overrides into its installation or source tree. The manager supports
search/filter, one explicit platform at a time, confirmed removal, folder
preview/import, and a read-only rescan. Custom PNGs remain higher priority
than the bundled pack.

PNG and JPEG inputs are currently decoded by the built-in safe codec set.
WebP magic is recognised but this build refuses it until the optional WebP
decoder is available; the original is never altered and the old custom image
is preserved. All accepted inputs use these limits:

- direct regular input files only; symlinks and non-files are rejected;
- magic bytes, not the extension, determine PNG/JPEG/WebP format;
- animation is refused;
- maximum encoded size 32 MiB, maximum dimension 8192px, and maximum decoded
  area 40 million pixels;
- images are aspect-fitted, centred, and atomically published as a clean
  1024×1024 PNG with no imported metadata;
- images smaller than the content area are not upscaled and produce a warning;
- malformed or failed replacements leave the preceding custom image intact;
- rendering caches length and modification time and is invalidated immediately
  after a managed change.

Bulk import accepts only exact lowercase canonical filenames with `.png`,
`.jpg`, `.jpeg`, or `.webp`; unknown names and duplicate targets remain in a
review list and are never silently assigned. Dry-run validates everything
without creating the managed directory. Category custom filenames
(`console.png`, `handheld.png`, etc.) remain a supported manual fallback when
placed in the managed folder and validated by Rescan.

The same operations are available through `archivefs-cli platform-artwork`.
No artwork command uses the network, database, ROM library, or emulator
profiles. Removing or restoring default artwork removes only ArchiveFS's
normalised managed copy, never the source selected during import.

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
1024×600 behavior. Hardware illustrations are aspect-fitted at 108 logical
pixels in a 142-pixel card, centred with transparent padding preserved. Compact
game rows use a recognisable 56-pixel thumbnail in a 64-pixel row.

Tests cover the complete registry and alias table, case normalization,
related-platform separation, category and Unknown fallbacks, custom override
precedence, malformed custom and bundled data, deterministic embedded decode,
absence of a bundled filesystem input, aspect fitting, card sizing, and
unchanged platform filtering/selection state. Pixel-perfect screenshots are
deliberately not used.
