# Library scan usability

## Mega Drive and Genesis loose ROMs

ArchiveFS uses the existing canonical platform name `MegaDrive`; Genesis is a
folder alias, not a second platform.

Loose `.gen` and `.smd` files are recognised case-insensitively because those
extensions are specific to this platform. Ambiguous `.md` and `.bin` files are
accepted only when the file is beneath, or the configured source itself is, an
exact recognised Mega Drive folder component. Accepted normalized folder
aliases include `megadrive`, `mega-drive`, `mega_drive`, `genesis`,
`sega-megadrive`, and `sega-genesis`. Matching is by a complete normalized path
component, so names such as `genesis-project` do not match. The filename never
provides folder evidence, and ordinary `README.md` files outside that context
remain ignored.

Loose ROMs are catalogued and searchable with exact path bytes preserved, but
are marked unsupported for ArchiveFS's archive-mount backend. Existing manual
platform assignments continue to outrank automatic detection.

Scanner ZIP handling is unchanged. A ZIP is catalogued as a ZIP container;
library scanning does not inspect or extract its members for platform
detection. Consequently a single `.md`, `.gen`, `.smd`, or `.bin` member does
not independently establish Mega Drive identity. The existing bounded Archive
Inspector remains read-only, but it is not used as a scanner identity shortcut.

## Scan summaries and Recently Found

Version 4 of the library database adds persistent scan-run counters for
unchanged files, unsupported extensions, and ambiguous platform files. Scan
summaries show added, updated, restored, missing, unchanged, skipped
unsupported, skipped ambiguous, and source-scan errors without creating one
activity event per file.

The `Recently Found` navigation entry is backed by the append-only
`archive_scan_observations` table. It shows only `added` observations from the
newest completed scan, in exact path order, and survives application restart.
A partial-success scan is still completed and exposes additions committed from
successful source folders. A failed database transaction has no committed scan
or additions and cannot replace the prior view. Updated existing entries are
reported in the summary but are not labelled newly found. Loading is bounded to
10,000 additions and reports truncation.

Recently Found uses the ordinary Library table, so search, platform, source,
present/missing filters, sorting, selection, and exact-path behavior remain
available. Navigation does not clear queue, mounts, filters, selections,
manual assignments, activity, transaction state, or History state.

## Scrollable pages

Settings, Doctor, About, Sources, Library Views, and History & Logs use the same
central vertical scrolling wrapper. It receives the central viewport remaining
after the activity panel, recalculates on resize or activity expansion, shows a
scrollbar whenever needed, and supports wheel, touchpad, Page Up, Page Down,
Home, and End. Table-oriented pages retain their existing dedicated scroll
regions; Cheats & Mods retains its existing explicit workspace scroll.

## Manual Nobara QA

1. Open Settings with Activity expanded and scroll to the final control with
   the mouse wheel.
2. Repeat with Page Down and End, then use Home to return to the top.
3. Resize the window smaller and repeat; collapse and reopen Activity and
   confirm the final control remains reachable.
4. Put synthetic `Alien 3 (USA, Europe).md` and another `.md` under an exact
   `megadrive` source/folder, plus an unrelated `README.md` outside it.
5. Rescan and confirm the summary counts the ROMs as added while the Markdown
   file is skipped ambiguous.
6. Open Recently Found, search for `Alien 3`, and confirm platform `MegaDrive`.
7. Confirm a `genesis-project/notes.md` file is not imported and uppercase
   `.MD`, `.GEN`, `.SMD`, and `.BIN` behavior follows the rules above.
8. Confirm a ZIP remains one ZIP catalogue entry and its members do not create
   loose-ROM entries.
9. Confirm active mounts, queue contents, selected archive, search/filters,
   manual platforms, activity, History, and RetroArch transaction state remain
   unchanged.
