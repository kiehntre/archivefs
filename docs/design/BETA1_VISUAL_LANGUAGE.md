# Beta 1 visual language

Status: implemented in `feature/beta1-visual-language` (PR #20), on top of the
PR #19 beta UX pass.

This pass locks the friendly visual language before the next hands-on beta
review. It changes presentation only - no backend, filesystem, DAT/rename/
organisation semantics, or provider behaviour changed.

## Primary Home concepts

The six major jobs are the primary Home destinations, each with a short
title, one short explanation, and a consistent glyph:

| Concept | Glyph | Home title | Page header |
|---|---|---|---|
| My Games | 🎮 | "My Games — Browse your games" | 🎮 My Games |
| Organise | 🗂️ | "Organise — Rename and tidy your library" | 🗂️ Organise |
| Check Library | 🩺 | "Check Library — Find problems" | 🩺 Check Library |
| Cheats & Mods | ❤️×99 | "Cheats & Mods — Find cheats and game enhancements" | ❤️×99 Cheats & Mods |
| Verify Games | 🧾 | "Verify Games — Check your games with DATs" | 🧾 Verify Games |
| Settings | ⚙️ | "Settings — Set up ArchiveFS" | ⚙️ Settings |

Secondary/admin destinations (Sources, Clean up my library, RomM, History &
Logs, Artwork, Mounts, About) stay available but render quieter and after the
primary set. Nothing is removed.

## Icon mapping

The same concept always uses the same glyph, defined once in
`crates/archivefs-gui/src/ui/icons.rs`:

- 🎮 My Games / Library (`GAMES`)
- 🗂️ Organise / Canonical Organisation (`ORGANISE`)
- 🩺 Check Library / Doctor (`CHECK`)
- ❤️×99 Cheats & Mods (`CHEATS`)
- 🧾 Verify Games / DAT verification (`VERIFY`)
- ⚙️ Settings (`SETTINGS`)
- 🖼️ Artwork (`ARTWORK`)
- 📜 History & Logs (`HISTORY`)
- 📂 Sources (`SOURCES`)
- 💿 Mounts (`MOUNT`)
- 🏠 Home, ⏱ Recent, 🎯 Selected, ℹ️ About, 🌐 RomM, 🧹 Clean up

Home cards and the matching page headers reference the same constants, so the
visual identity cannot drift between the two.

## Cheats & Mods visual

The generic puzzle-piece identity was replaced with the **❤️×99** cheat-game
concept. One restrained retro cheat-code motif (`↑ ↑ ↓ ↓ ← →`) appears once
on the Cheats & Mods page header as decoration only; it is never the primary
label and is not repeated across the app.

## Retro-character rules

Restrained retro references only:

- ❤️×99 lives counter and the subtle cheat-code arrows;
- the gamepad/cartridge/disc glyph language from the icon set;
- small playful empty-state symbols.

Avoided: fake CRT scanlines, pixel fonts for body text, flashing neon,
arcade-wallpaper decoration. This is modern software with a retro soul.

## Empty states

Major empty states now explain the next step with the concept's glyph:

- 🎮 "No games yet — Add a source or scan your library."
- 🧾 "No DATs added — Add a DAT catalogue to verify your games."
- ❤️×99 Cheats & Mods, 🗂️ Organise, and 🔍 "No matching archives" follow the
  same pattern.

## Status visuals

`status_badge` now prefixes every label with a tone cue (✓ Ready / !
Needs attention / ? Unknown / × Blocked / ▶ Active / i Info). Status is never
carried by the glyph alone - the word always remains.

## Accessibility / fallback

- Icons are always accompanied by the text title and explanation; a missing
  glyph on some system never removes meaning, and no navigation depends on an
  emoji-only button.
- Compound glyphs use plain glyph+text composition (❤️ + ×99) rather than
  fragile font hacks; no new fonts are bundled.
- Compact width (≈700px): primary cards stack and wrap; a test renders Home
  at 700px and asserts every primary title + icon is present without panic or
  clipping.

## Deliberately deferred

A hand-drawn illustration / proper icon set, a full theme pass, and richer
per-page artwork are intentionally left to a later illustration pass after the
next beta review. This pass establishes the visual language with the existing
icon component system only.
