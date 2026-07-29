# RetroArch cheat installation workflow

How a user goes from "this archive is selected" to "these cheats are in
RetroArch", and what each stage is allowed to do.

This document covers the end-to-end install path added on
`sonnet-retroarch-cheats-working`. The read-only catalogue indexing,
retrieval, and inventory layers it builds on are documented separately in
`RETROARCH_CHEAT_CATALOGUE.md`, `RETROARCH_CHEAT_SOURCES.md`, and
`RETROARCH_ARTIFACT_INVENTORY.md`.

## Why this was needed

Before this milestone the only thing the trusted-catalogue path could do
was copy a whole catalogue `.cht` file verbatim, and it did so for every
exact-or-strong match it found, without ever showing the user a candidate.
Three things made the workflow unusable end to end:

1. **No candidate list.** `match_cheat_game_record` answers the *indexing*
   question ("which library archive does this catalogue record belong
   to?") and deliberately refuses to choose on a tie. The install path
   needs the mirror-image answer, and an ambiguous match simply produced
   nothing to choose between.
2. **No cheat codes.** Both existing `.cht` readers keep metadata only -
   counts, descriptions, enable flags - and never retain a code body. A
   subset of a file therefore could not be generated at all.
3. **A destination that did not match RetroArch's own layout.** The
   canonical platform short name (`NES`) is not what a RetroArch cheat
   root is organised by.

## The stages

Each stage is a separate module, and each one refuses rather than guesses.

### 1-3. Archive, profile, catalogue

Unchanged. The archive comes from Library, the profile from RetroArch
profile discovery (never a default this project invents), and the
catalogue from a verified, digest-checked trusted-source snapshot.

### 4. Candidate matches - `cheat_candidates`

`build_cheat_candidates` evaluates every catalogue record against the one
selected archive and keeps *all* the evidence, rather than stopping at the
first tier that hits. Each candidate carries its catalogue-relative path,
display name, platform, region, revision, classification, an ordered
0-1000 score, structured evidence, cheat count, and two independent
eligibility flags.

| Classification | Meaning | Installable | Auto-selected |
| --- | --- | --- | --- |
| Verified exact | serial, content hash, or title+platform+region agree | yes | only when uniquely best |
| Strong | title and canonical platform agree | yes | never |
| Ambiguous | tied with another candidate at the top score | yes, after an explicit choice | never |
| Weak | title agreement with no platform corroboration | yes, after an explicit choice | never |
| Different platform | both sides declare a platform and they disagree | **never** | never |
| Unsupported | targets another emulator, or did not parse cleanly | **never** | never |

Evidence kinds are stable identifiers, and one is only emitted for a
comparison actually performed against data **both** sides declared. An
archive with no region produces no region claim at all, rather than a
"not checked" line that would read like a finding. Mismatches are
evidence too: they are shown, and they never raise the score.

The list is capped (25 by default), reports the pre-cap total, and takes a
query filter applied before the cap so a truncated list stays searchable.

### 5. Evidence

The chosen candidate's identity, catalogue path, file digest, and the
same evidence list, so the reason for the match survives the choice.

### 6. Individual cheats - `cht_document` and `CheatSelection`

`cht_document` is the first `.cht` reader that retains code bodies, and
therefore the first that can write a file back out. It:

- never panics on catalogue input (bounded fields, checked indexes, no
  slicing on a non-char boundary);
- reports unsupported encodings (UTF-16 BOM, invalid UTF-8) rather than
  lossily decoding, and tolerates a UTF-8 BOM;
- preserves leading comments, global keys such as `cheat_delay`, and
  unknown per-entry fields such as `cheatN_handler`;
- reports malformed lines, malformed and out-of-range indexes, duplicate
  fields, declared-count mismatches, and non-contiguous indexes as
  warnings, keeping the rest of the file usable;
- marks an entry unselectable when it has no code, an empty code, an
  oversized value, or a control character in a value.

The picker starts with **nothing selected** - "no cheats chosen yet" is a
real state to resolve, not something to guess past. Each entry's initial
`enabled` comes from the source's own `cheatN_enable`, keeping *included
in the installed file* separate from *active as soon as RetroArch loads
it*. An unsafe entry cannot become selected through any path: the toggle
refuses it, `select_all` skips it, and `resolve` refuses it again.

Apply stays unavailable until at least one usable cheat is selected.

### 7. Generation and preview - `cheat_install_plan`

`render_cht_file` is a pure function of its input: contiguous indexes from
zero, a `cheats = N` header that always agrees with the entries written,
preserved per-entry fields re-emitted under the new index, and exactly one
trailing newline.

The only content the renderer alters is an interior double quote.
RetroArch's own `config_file` reader has no escape syntax inside a quoted
value, so an unescaped quote would truncate the value it sits in. That one
substitution - `"` becomes `'` - is deterministic, flagged at parse time
as `cht_entry_quote_normalized`, and never written back to the catalogue.

The rendered file is staged atomically into a private directory, which
then acts as the "approved source root" the existing transaction machinery
already understands. The staging root exists because that machinery
installs *files*, by digest, from an approved root - and a generated
subset has no file in the catalogue to point at. **The catalogue itself is
only ever read.**

### Destination

`<profile cheat root>/<platform directory>/<name>.cht`.

- **Platform directory.** A real RetroArch cheat root is laid out by
  libretro *database* name (`Nintendo - Nintendo Entertainment System`),
  not by this project's canonical short name (`NES`). Resolution therefore
  prefers an existing subdirectory of the profile's own cheat root whose
  name resolves to the same canonical platform, and falls back to the
  canonical name only when none exists. Only real directories count, only
  their own names are trusted, symlinked entries are skipped rather than
  followed, and ties break by name. An unrecognized platform blocks
  installation entirely rather than passing untrusted text through a
  sanitizer.
- **Filename.** Taken from the strongest available identity in order:
  content basename, then playlist name, then catalogue name. An unsafe
  value is *skipped*, never laundered into a safe-looking one. Which
  source was used is always reported before confirmation.
- **Safety.** Traversal, symlinked parents, symlinked destinations, and
  anything resolving outside the profile's cheat directory are rejected by
  the same `assess_destination` checks every other adapter uses.

### 8-9. Confirm, apply, result, rollback

The shared transaction pipeline, unchanged: pre-state recorded, backup
retained before any replacement, atomic write, content verification, one
journal per run, and a journal-backed rollback. Cancellation produces no
filesystem changes at all. Replacement needs its own separate approval on
top of general confirmation. The result view offers **Roll back this
install**, which runs the same rollback History & Logs already uses.

## How RetroArch finds the installed file

RetroArch does **not** auto-load a per-game cheat file by name. The
installed file is reached through:

> Quick Menu → Cheats → **Load Cheat File (Replace)** → the platform
> directory → the installed `.cht`

`cheat_database_path` in `retroarch.cfg` is where that browser opens, and
it is exactly the profile cheat root installation writes into - which is
why installing into the directory RetroArch already has matters, rather
than creating a parallel one.

With `apply_cheats_after_load = "true"` (RetroArch's default), the cheats
in the loaded file that carry `cheatN_enable = true` become active
immediately. That is precisely the distinction the picker's "Active on
load" checkbox controls.

## Activity and history

Separate events are recorded for catalogue retrieval, match completion,
candidate opening, preview creation, install start, install completion,
install failure, install cancellation, and rollback. Cheat **codes** are
never logged; descriptions and counts are.

## Manual proof checklist

Automated tests do not prove this works in RetroArch. Run the release GUI:

```
/home/davedap/archivefs-fable/target/release/archivefs-gui
```

1. **Selected archive** - Library → right-click a game whose platform
   RetroArch has a cheat directory for → *Cheats & Mods*. Confirm the
   archive name shown at the top.
2. **Profile** - pick the RetroArch profile under "Choose a RetroArch
   profile".
3. **Catalogue** - retrieve or reuse the trusted catalogue snapshot.
4. **Candidate list** - confirm the "Candidate matches" section lists real
   `.cht` files with a classification badge on each.
5. **Evidence** - confirm each candidate shows why it matched, and that a
   different-platform candidate is shown as blocked with no button.
6. **Choose** - click *Use this cheat file* (or confirm a verified-exact
   match was preselected).
7. **Individual cheats** - confirm the real cheat names appear, tick two
   or three, try *Select all* and *Clear all*, and confirm *Preview the
   installed file* is unavailable with nothing ticked.
8. **Preview** - confirm the destination path, the platform-directory and
   filename sources, the cheat counts, the new file's SHA-256, and the
   full file contents under technical details.
9. **Confirm** - *Review exact apply plan* → *Confirm and apply exact
   plan*.
10. **Result** - confirm success, the operation ID, and the journal path.
11. **Installed file** - in a terminal, `cat` the destination path shown
    in the preview. Confirm it contains only the cheats you ticked, with
    `cheats = N` matching and indexes contiguous from zero.
12. **RetroArch** - launch RetroArch, load the same content, then Quick
    Menu → Cheats → Load Cheat File (Replace) → the platform directory →
    the installed file. Confirm the cheat list shows your descriptions and
    that a cheat marked "Active on load" is enabled.
13. **History** - confirm the Cheats & Mods activity list and History &
    Logs both show the install, with archive, candidate, cheat count,
    destination, result, and timestamp.
14. **Rollback** - press *Roll back this install* and confirm the previous
    file (or the absence of one) is restored.
