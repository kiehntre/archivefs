# Beta 1 UX & Discoverability review

Date: 2026-08-08. Branch: `feature/beta1-ux-discoverability` (== origin/main base).

This records the first hands-on beta session's UX problems and what PR #19
changed, plus the items deliberately left for a later pass. Everything here is
presentation and wording; no safety or matching semantics changed.

## Menus / toolbar wording

- **Problem:** the Library menu used jargon ("Refresh Live Snapshot",
  "Refresh database status") with no explanation.
- **Old:** `Refresh Live Snapshot`, `Refresh database status`, `Select all
  visible`, `Clear selection`, `Diagnostics`, `Doctor checks`, `Platform
  Aliases`, `Database Status`.
- **New:** `Refresh` (tooltip "Refresh ArchiveFS's current view of your files
  without running a full scan."), and every non-obvious menu action now has a
  concise `.on_hover_text` tooltip. Nothing was removed; advanced actions are
  still all present, just explained.

## File-safety wording

- **Problem:** safety text was long and developer-facing.
- **Old:** "ArchiveFS never renames, moves, deletes or rewrites your ROMs. An
  audit reads files and reports what it found; nothing is changed…" and the
  long planning-only paragraph.
- **New:** the DAT Sources and rename-planning banners now lead with "Your
  files won't be renamed unless you approve it." The longer explanations stay
  available as a small muted line, not front and centre. The safety claim is
  unchanged.

## Rename / organise discoverability

- **Problem:** rename was only reachable from inside DAT Sources; the beta
  user could not find it.
- **New:** Home has a "Clean up my library" card → DAT Sources (where rename
  planning lives), and the existing "Organise my library" (Canonical
  Organisation) card now offers a secondary "Review filename suggestions" link
  to the same DAT rename workflow. No second rename implementation was added.

## DAT region / language "Any" preference

- **Problem:** an empty preference list meant "all equal", shown as
  "None (all equal)" / "none - all regions equal".
- **New:** the effective summary renders "Any" ("Region preference: Any"), the
  editor empty state says "Any region — no preference", and an explicit
  **Any region / Any language** button clears the ordering back to no
  preference. Persisted semantics (empty = no preference) are unchanged;
  matching policy behaviour is untouched.

## Cheat Sources grouping

- **Problem:** several GameCube/Wii sources (Dolphin GameSettings, Dolphin
  catalogue, GameHacking) looked like duplicates.
- **New:** sources are grouped under per-emulator section headers (Dolphin,
  RetroArch, PCSX2, Xenia, …). Presentation only: every provider is still
  listed, enable/disable and priority remain per source, and no provider is
  merged.

## Icons (friendly visual language)

- **Problem:** navigation was text-only.
- **New:** a small consistent Unicode glyph set (`ui/icons.rs`) is used on
  every Home card and every major page header. The same concept always uses
  the same glyph (Library/Games = 🎮, Sources = 📂, Cheats = 🧩, Mount = 💿,
  Doctor = 🩺, Settings = ⚙️, …), and several empty states gained a leading
  icon. Icons always accompany the text label and never replace it. No image
  assets were added; a larger illustration pass can follow after more beta
  feedback.

## Artwork "Deferred" states

- **Problem:** bare "Deferred" labels gave no reason.
- **New:** the identity status now renders "Not available yet" instead of
  "Deferred", and the Dolphin identity detail already explains the concrete
  reason ("cannot yet read an exact Game ID without decompressing…").
- **Image-picker freeze:** the platform-artwork "Choose image" button ran
  `rfd::FileDialog::pick_file()` on the egui UI thread, which freezes the
  frame while the native dialog is open. Fixed in a small scoped way: the
  dialog now runs on a background thread and the result is drained via a
  channel each frame, so the UI keeps rendering. Not reproducible headlessly
  here; the fix is the standard blocking-dialog-off-the-UI-thread pattern.

## Doctor benign findings

- **Problem:** "Items needing no archive mount: 840" with hundreds of loose
  ROM examples.
- **New:** the compact group now reads "{count} loose ROMs are healthy" with
  "These files are used directly and do not need ArchiveFS mounting.", then
  bounded examples and a "Show all N findings" expansion. Raw findings and the
  copy-report export are unchanged.

## .cue companion files

- **Outcome:** the library model does not pair BIN/CUE into one game, but it
  already classifies `.cue`/`.m3u` sheets as disc-image companions
  (`InspectorEntryClassification::Documentation`), never as likely game
  content and never as an independently missing game. No guessing was added;
  a regression test pins this confirmed behaviour. Limitation documented: a
  `.cue` is not grouped under its `.bin` game (that would need a companionship
  model - a later PR if wanted).

## BSFree clarity

- **Problem:** BSFree's capability label rendered "Local, read-only", which
  understated that it is browse/reference-only.
- **New:** browse-only sources are labelled **"Browse only"**. BSFree's
  download/validate/enable/disable/remove/import controls and its "no command
  installs cheats" wording are unchanged; no installation engine was added.

## DAT audit result grouping

- **Problem:** a run over many unreadable files dumped repeated "symlink
  refused…" lines.
- **New:** unhashed (name-only) files are grouped by reason with an exact
  count ("N symlinks could not be hashed"), a plain-language reason, bounded
  examples, and a "Show all N" expansion. Raw details remain available and the
  underlying data is never discarded.

## Page introductions

Major pages now open with a one-to-two-line purpose (e.g. DAT Sources: "Use
DAT catalogues to check and identify your ROMs."; Canonical Organisation:
"Preview where your games would go. Nothing moves until you approve it.";
Doctor: "Check your ArchiveFS setup and find problems. Running it changes
nothing.").

## Deferred UX items (follow-up)

- A dedicated hand-drawn icon/illustration set and full theme pass (the glyph
  set here is intentionally minimal).
- A BIN/CUE companionship model to group a `.cue` under its game.
- Cross-filesystem organisation and canonical symlink creation (already
  deferred in the Canonical Organisation design).
- Loose-ROM / .cue wording in the DAT audit's "Not in DAT" verdict (currently
  accurate but terse).

## Tests

Regression coverage added for: simple Refresh menu wording, the exact safety
promise, Home rename/organise discoverability, Any-region / Any-language
clearing, effective-policy "Any" rendering, cheat-source grouping preserving
every provider, BSFree "Browse only", Deferred → "Not available yet", the
friendly loose-ROM Doctor summary, grouped symlink diagnostics with an exact
count and bounded examples, .cue companion classification, Home-card and
page-header icons alongside labels, and page intro brevity.
