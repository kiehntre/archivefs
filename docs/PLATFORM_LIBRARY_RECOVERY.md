# Platform and library recovery

## Proven pre-change root causes

The source-to-Cheats path was traced before implementation:

`Config::source_folders` → `ArchiveScanner::scan_source` →
`Archive::from_path_in_root` / `archive_kind_in_root` →
`scan_and_persist_folders` → `archives` and `platform_assignments` rows →
`load_database_snapshot_at` → `build_display_rows` → Library and
`show_cheat_archive_picker`.

The observed failures have four concrete causes:

1. `archive_kind` recognizes ZIP, 7z and RAR containers, plus a narrowly
   folder-gated set of Mega Drive files. RVZ, ISO, GCM, GCZ, WBFS and CISO
   are rejected before an `Archive` exists. They therefore cannot produce a
   database row, merged Library row, or Cheats & Mods candidate.
2. A source containing the direct GameCube images above consequently reports
   zero archives even though its directory is enabled, readable and contains
   games. The count is produced by discovery, before any GUI filtering.
3. The built-in alias registry already contains `gcn` and common GameCube
   names, but folder matching inspects only directories strictly beneath the
   configured source root. A source rooted at `/mnt/games/roms/gcn` excludes
   its own `gcn` component. A root named
   `Nintendo - GameCube (2018-08-25 20-44-48) (America)` is normalized as one
   long token and does not exactly equal `nintendogamecube`; collection date
   and region suffixes are not stripped.
4. The Cheats & Mods chooser consumes the same merged Library rows but
   intentionally lists only live rows. `Animal Crossing (USA).zip` becomes a
   live row through the container allow-list. `ZooCube (USA).rvz` never enters
   discovery, so it is absent rather than merely filtered out.

Platform precedence is also inconsistent with the recovery goal: legacy
known-path/title heuristics are evaluated before the central folder-alias
fallback. Source configurations have no platform field, so there is no
persisted source-level evidence or safe source-wide reclassification path.

## Safety boundaries

Direct images are catalogue items and game contexts. Scanning remains bounded
and read-only: it never mounts, renames, converts or writes a game image.
Unsupported exact identity (including RVZ until a bounded decoder exists) is
retained as honest missing evidence rather than used to hide the item.

## Implemented recovery rules

- ZIP, 7z and RAR behavior is unchanged. ISO, GCM, GCZ, RVZ, WBFS and CISO
  are also catalogued as direct game images. They are not sent to the archive
  mount backend.
- Platform evidence is applied in this order: saved per-entry override,
  bounded format/header identity, saved source assignment, exact normalized
  folder alias, filename/path evidence, Unknown.
- The central alias registry includes `gcn`, `gc`, `gamecube`,
  `nintendo gamecube`, `nintendo - gamecube`, `xbox360`, `xbox 360`, `x360`,
  `microsoft xbox 360`, `ps2`, `playstation 2`, `sony playstation 2`, `ps3`,
  `playstation 3`, `sony playstation 3`, `xbox`, `xbox original`, and
  `microsoft xbox`, alongside all previously supported platforms. Matching is
  case-insensitive and separator/punctuation-insensitive, and ignores safe
  trailing collection dates and parenthesized region/date suffixes.
- A Sources-row **Assign platform** action previews the current Unknown count,
  persists the choice, and rescans. Incompatible direct images remain visible
  and produce a warning; stronger header or manual identity is preserved.
- Library, Mount, and Cheats & Mods share the session platform selection.
  Library exposes Unknown deliberately, clears stale focus when switching,
  and keeps the platform strip visible above the compact filters and rows.

## Manual proof still required

After automated verification, manually check without modifying either source:

- `/mnt/games/roms/gcn/ZooCube (USA).rvz`
- `/mnt/nvme2/remote-decypharr-mnt/games/Nintendo - GameCube (2018-08-25 20-44-48) (America)`

Automated tests use temporary fixtures only and do not access the live ROM
collection. Manually verify:

1. The remote source scan reports more than zero items.
2. Its Sources row can be assigned GameCube and shows the Unknown preview.
3. `ZooCube (USA).rvz` appears in Library.
4. GameCube shows both Animal Crossing and ZooCube.
5. The Cheats & Mods chooser shows both items and starts on GameCube for
   Dolphin.
6. ZooCube states that exact RVZ Game ID extraction is unavailable instead of
   disappearing.
7. At normal desktop size and 1024×600, counters remain horizontal and at
   least two complete archive rows are immediately visible with no bottom
   overlay.
8. The Unknown count decreases after the confirmed source assignment and
   rescan; any incompatible items are reported rather than silently assigned.

## Follow-up: registry expansion and direct GameCube identity

A second pass (see
[`PLATFORM_REGISTRY_AND_DIRECT_IDENTITY.md`](PLATFORM_REGISTRY_AND_DIRECT_IDENTITY.md)
for full detail) addressed two proven gaps left by the recovery above:

1. **Thousands of Unknown entries with clear folder evidence.** The single
   `FOLDER_PLATFORM_ALIASES` registry this document already established had
   no canonical platform at all for Virtual Boy, Sharp X68000, NEC
   PC-8801/PC-9801, Nintendo 3DS, PC Engine CD, ZX Spectrum's `zxs`
   abbreviation, Commodore 128, or VIC-20 - so a folder named exactly
   `virtualboy` or `sharp-x68000` still fell through to Unknown no matter how
   clear the evidence was. These are now first-class aliases in the same one
   table (never a second table), plus a separate, narrower
   `FOLDER_PREFERRED_EMULATOR_ALIASES` table so `fbneo`/`mame`/`fba` folder
   names classify as the `Arcade` *hardware* platform while still recording
   which *emulator* the folder name implies.
2. **RVZ (and other direct GameCube images) stuck at "Waiting for verified
   identity".** Identity inspection was not actually hanging - it reached a
   final `GameIdentityReport` for RVZ (`Deferred`) and for `.gcz`/`.ciso`
   (previously `Unsupported` by omission, a bug fixed here) - but the
   *downstream* Dolphin provider-fetch gate required a `Verified` Game ID
   before it would ever leave `NotLoaded`, so the beginner status stayed on
   "Finding compatible cheats" forever. RVZ and the GameCube/Wii `.ciso`
   format now have real bounded direct-header identity readers (see the
   companion document for the exact byte layout each uses), and the
   workflow state machine now has a distinct terminal `IdentityUnavailable`
   status for whatever remains undecodable (`.gcz`, `.wbfs`), so the page
   never spins indefinitely regardless of whether a given format is
   supported yet.

Both fixes also unified the three previously-duplicated, hardcoded GUI
platform lists (Sources "Assign platform", Mount filter, Library filter)
into one live-data-driven strip, so a platform only ever appears when the
registry recognises it *and* the library actually contains a non-zero count
of it - see the companion document's "Full platform filtering" section.
