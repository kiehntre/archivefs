# Platform artwork

Status: visual picker implemented on `feature/gui-navigation-reset`;
responsive sizing and safe custom rendering completed on
`feature/gamer-view-finishing-pass`.
Covers Gamer View's visual platform picker - see
`docs/GUI_NAVIGATION_RESET_DESIGN.md` for the surrounding navigation
design this sits inside.

## Policy

- All built-in platform artwork is **original ArchiveFS project artwork**,
  drawn specifically for this feature. No third-party artwork, theme
  pack, or icon set was copied, traced, or adapted - not from ES-DE,
  RetroPie, Batocera, RomM, RetroArch themes, Wikimedia, or any other
  source.
- No manufacturer logo, product photo, or trademarked design is
  reproduced. Console-shaped glyphs (e.g. the GameCube/PlayStation
  2/Xbox-inspired shapes) are deliberately abstract geometric
  interpretations - a cube, a tower, a circle with crossed bands - not
  reproductions of any real product's shape, logo, or branding.
- **No network access of any kind is used to obtain, update, or check for
  platform artwork.** Every built-in asset ships inside the repository;
  every mapping from a platform to an asset is a static, compile-time
  Rust table (`platform_asset_id`/`platform_asset_category` in
  `crates/archivefs-gui/src/main.rs`). A custom artwork directory (see
  below) is read from the local filesystem only.
- "Do not block the feature on creating a unique image for every obscure
  platform" - a small, closed set of **category fallbacks** (console,
  handheld, computer, arcade, optical-disc, cartridge, unknown) covers
  every platform that doesn't have a dedicated asset. An unmapped
  platform is not a bug; it correctly and honestly falls back to a
  category, or to Unknown if even the category is unrecognised.

## Built-in asset format and rendering

`crates/archivefs-gui/assets/platforms/*.svg` - one SVG file per asset,
`<lowercase-asset-id>.svg`. Total bundle size: **4,303 bytes** (10 files,
~430 bytes average) - not embedded as Rust byte arrays; they exist as
plain files in the repository for review and licensing clarity. They are
not parsed at runtime.

Gamer View's platform shelf renders a small **native vector glyph** drawn
directly with egui's own painter (`paint_platform_glyph`/
`paint_platform_glyph_at` in `main.rs`), keyed by the same asset
identifier the SVG files are named after - not a rasterized copy of the
on-disk SVG content.

The on-disk SVG files remain the canonical built-in artwork source and
licensing record. Runtime SVG parsing remains deferred because it would
introduce an XML/vector rendering stack (`resvg`/`usvg`/`tiny-skia`) for
content that is currently satisfied by the native fallback. Custom
artwork uses the deliberately narrower PNG path described below.

## Responsive shelf behaviour

- The shelf is always one horizontal row and remains horizontally
  scrollable; it never wraps or increases its fixed height.
- Cards scale between 96 and 124 logical pixels according to the viewport.
  They cannot collapse below the readable minimum or expand without bound.
- The count always remains on its own line. A platform name that does not
  fit is shortened once with an ellipsis; the hover text and accessible
  name retain the complete platform name and count.
- `All` and `Unknown` remain unabridged at the minimum card width.
- Selection uses egui's selected button state and keyboard focus remains
  the standard button focus ring.

## Fallback categories

| Category | Asset id | Used for (examples) |
|---|---|---|
| Console | `console` | GameCube, Wii, Switch, N64, NES, SNES, MegaDrive, PS2, PS3, Xbox, Xbox 360, Saturn, Dreamcast, and others with no dedicated asset |
| Handheld | `handheld` | Game Boy family, Nintendo DS, Nintendo 3DS, PSP, PlayStation Vita, Atari Lynx, Neo Geo Pocket family, WonderSwan family, N-Gage |
| Computer | `computer` | PC, DOS, ScummVM, Amiga, Atari ST, Commodore 64, ZX Spectrum, Acorn Archimedes, Sharp X68000, NEC PC-8801/9801, MSX family |
| Arcade | `arcade` | Arcade |
| Optical disc | `optical-disc` | Amiga CD32 (a narrow category today - most CD-based consoles above are classified as Console for now, since that's how players commonly think of them) |
| Cartridge | `cartridge` | Atari 2600/5200/7800, Atari Jaguar, Vectrex, TurboGrafx-16, PC Engine |
| Unknown | `unknown` | Any platform this build doesn't recognise at all, and every row where platform detection itself found nothing (`unknown_platform: true`) |

Classification lives in `platform_asset_category()` in `main.rs` and is
covered by `platform_asset_id_falls_back_to_category_for_unmapped_specific_platforms`
and related tests.

## Dedicated (specific) assets

Only platforms with real, original artwork get their own asset id instead
of a category fallback:

| Platform | Asset id |
|---|---|
| GameCube | `gamecube` |
| PS2 | `playstation2` |
| Xbox, Xbox 360 | `xbox` |

Adding a fourth dedicated asset means: add the `.svg` file to
`crates/archivefs-gui/assets/platforms/`, add its manifest row below, add
a native-vector case to `paint_platform_glyph_at`, and add a match arm to
`platform_asset_id`. No other code changes needed - everything else
(the shelf UI, the accessible-label logic, custom-override resolution)
is already generic over asset id.

## Custom artwork directory (Advanced View → Settings → "5. Platform
artwork")

An optional, session-configurable directory of the user's own PNG files,
named by canonical platform identifier in lowercase
(`gamecube.png`, `playstation2.png`, `xbox.png`, or a category id like
`console.png`). The full supported mapping is the asset-id column in the
tables above: `console`, `handheld`, `computer`, `arcade`, `optical-disc`,
`cartridge`, `unknown`, `gamecube`, `playstation2`, and `xbox`.

The matching PNG's actual pixels are decoded and rendered in Gamer View.
SVG custom files are not parsed and therefore fall back to the built-in
glyph; this is intentional and is never presented as SVG support.

Safety boundaries:

- Files must be regular local files directly inside the configured
  directory. Symlinks, invalid asset ids, missing files, and other file
  types are rejected.
- Maximum encoded file size: **1 MiB**.
- Maximum dimensions: **1024×1024 pixels**; decoded allocation is bounded
  to **4 MiB**.
- Only PNG decoding is enabled. Malformed or unsupported content is not
  partially rendered and cannot crash the interface.
- Every rejection falls back to the original built-in vector glyph while
  the platform's text name, count, focus, and keyboard activation remain.

Successful textures and failed decode results are cached by asset id plus
the configured directory, file length, and modification time. Changing
the configured directory clears the cache immediately. Changing a file's
length or modification time causes it to be decoded again on the next
render; unchanged files are not reparsed per frame.

- **Never copies the custom file anywhere.** ArchiveFS only ever
  references the path the user configured; nothing is written into
  ArchiveFS's own storage or config directory.
- Persisted as a single path string in its own small file
  (`~/.config/archivefs/platform_artwork_directory.txt`), written only
  when the setting is explicitly changed - the same persistence pattern
  `docs/GUI_NAVIGATION_RESET_DESIGN.md`'s `GuiMode` preference already
  uses, and for the same reason (one dedicated file per preference, never
  a shared one that risks one preference's write corrupting another's).
- No automatic ES-DE (or any other launcher's) theme discovery is
  implemented - the user must point ArchiveFS at a directory explicitly.

## No-network guarantee

Grep-verifiable: no HTTP client, URL loader, download, or "check for
updates" logic exists anywhere in the platform-artwork code path. The
decoder receives only bytes opened from the explicitly configured local
directory. No automatic discovery or download is performed.

`image` 0.25.10 is now a direct GUI dependency with only its `png` feature
enabled. It was already present in the resolved dependency graph, adds no
network capability, and is licensed `MIT OR Apache-2.0`, compatible with
ArchiveFS. No SVG rendering dependencies were added.

## Storage impact

10 SVG files, 4,303 bytes total on disk. None are currently compiled into
the binary (the native-vector rendering path doesn't read them at
runtime) - see "Built-in asset format and rendering" above.

## Asset manifest

| Asset filename | Platform / category | Creator | Licence | Attribution required |
|---|---|---|---|---|
| `console.svg` | Console (category fallback) | ArchiveFS project (original) | Same licence as this repository's own source | No - original project artwork |
| `handheld.svg` | Handheld (category fallback) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `computer.svg` | Computer (category fallback) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `arcade.svg` | Arcade (category fallback) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `optical-disc.svg` | Optical-disc system (category fallback) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `cartridge.svg` | Cartridge system (category fallback) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `unknown.svg` | Unknown-platform fallback | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `gamecube.svg` | GameCube (dedicated, abstract) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `playstation2.svg` | PS2 (dedicated, abstract) | ArchiveFS project (original) | Same licence as this repository's own source | No |
| `xbox.svg` | Xbox / Xbox 360 (dedicated, abstract) | ArchiveFS project (original) | Same licence as this repository's own source | No |

Every asset is original project artwork with no attribution obligation.
If a future contribution adds third-party or externally-sourced artwork,
add its row here with the real creator, licence, and attribution
requirement **before** merging it - this table is the single place that
decision must be recorded.

## Process for adding a new platform asset

1. Confirm the artwork is original, or that its licence explicitly
   permits redistribution in this project, and that it doesn't reproduce
   a manufacturer's logo, trademark, or copyrighted product design.
2. Add `crates/archivefs-gui/assets/platforms/<asset-id>.svg`.
3. Add a row to the manifest table above with the real creator/licence.
4. Add a `paint_platform_glyph_at` match arm for `<asset-id>` (native
   vector rendering - see "Rendering approach" above for why this is
   still the current mechanism).
5. Add or extend a `platform_asset_id`/`platform_asset_category` match
   arm so the relevant platform(s) resolve to `<asset-id>`.
6. Add a test alongside the existing `platform_asset_id_*` tests in
   `main.rs` proving the new mapping.

## Unresolved / follow-up

- **True SVG rasterization is not wired up.** The on-disk SVG files remain
  the canonical built-in source assets. Users who want custom artwork must
  supply PNG files under the documented limits.
- No automated visual/pixel-diff test exists for the glyphs themselves
  (deliberately - "do not create fragile screenshot-pixel tests unless
  the existing test approach supports them reliably," and it does not).
  Coverage instead proves mapping, responsive bounds, safe decoding,
  cache invalidation, texture use, and built-in fallback behaviour.
