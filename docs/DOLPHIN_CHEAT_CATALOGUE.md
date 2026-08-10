# Dolphin cheat catalogue

The Dolphin cheat catalogue is a locally cached, offline-searchable index of
Gecko cheat definitions built from the official
[`dolphin-emu/dolphin`](https://github.com/dolphin-emu/dolphin) upstream
repository's `Data/Sys/GameSettings` directory - the same GameCube/Wii
GameSettings dataset Dolphin itself ships with releases.

It exists alongside, not instead of, the original per-game upstream provider
(`dolphin_gecko_provider`): that provider still fetches one exact game's
`.ini` from `master` on demand. The catalogue answers the same question for
every game at once, once, so selecting a game afterwards is a local lookup,
not a network request.

## Upstream source

- Repository: `https://github.com/dolphin-emu/dolphin`
- Directory indexed: `Data/Sys/GameSettings/` only - nothing else in the
  repository (source code, assets, documentation) is ever decompressed.
- Licence: GPL-2.0-or-later (the repository's own `COPYING` file).
- Attribution: "Gecko definitions from the Dolphin Emulator upstream
  `Data/Sys/GameSettings` dataset."

Every catalogue records the exact resolved commit it was built from, the
immutable archive URL, and the archive's SHA-256. A branch name alone (e.g.
`master`) is never treated as an installed catalogue identity.

## What the catalogue contains, and what it does not claim

For each `Data/Sys/GameSettings/<GAMEID>.ini` file whose filename is an exact
six-character GameCube Game ID, the catalogue records:

- the Game ID and, when the file's own leading comment declares it, a title
- the region the Game ID encodes
- every parsed Gecko code: name, complete hex-pair body, notes, and whether
  it is safe to offer (malformed lines and ambiguous duplicate names are
  blocked, never silently repaired or deduplicated by guessing)
- per-file warnings (no `[Gecko]` section, no codes, blocked entries)
- the source path inside the archive

Upstream does not declare which disc revision a Gecko entry supports, so
every entry's revision applicability is `Uncertain` - offered, but flagged
for review, exactly like the single-game provider already does.

Files whose name is not an exact six-character Game ID (Dolphin's shorter
wildcard-prefix filenames, which match multiple discs at once) are
intentionally out of scope: the catalogue's lookup contract is exact-Game-ID
only, so a file that does not declare one game's ID cannot be indexed by one.
These are counted, honestly, as "non-matching files skipped" in the
catalogue's own statistics - never silently dropped without a trace, and
never reinterpreted as belonging to a specific game.

**The catalogue never claims that every upstream `GameSettings` file
contains gameplay cheats.** Many files exist only for `[Core]`/`[Video_*]`
compatibility settings with no `[Gecko]` section at all; the catalogue
reports the real, measured counts (games inspected, games with usable Gecko
codes, total usable entries, skipped/malformed files) rather than a claim
that the whole dataset is "cheats."

## Catalogue versus your Dolphin profile

The catalogue is a read-only reference dataset. It is never copied into a
Dolphin profile's `User/GameSettings` directory, and downloading, updating,
or removing it never touches your Dolphin installation, its `GameSettings`
files, or codes you have already installed there.

Installing a cheat still goes through the existing safe Dolphin workflow
unchanged: an exact profile is selected, the destination `.ini` is loaded,
only the `[Gecko]`/`[Gecko_Enabled]` sections are surgically edited, and the
write goes through the same backup/journal/verification/Undo pipeline as
every other Dolphin install - whether the codes being installed came from
the catalogue or the older single-game provider.

## Download method

A single archive, pinned to the resolved commit, is downloaded from
`https://codeload.github.com/dolphin-emu/dolphin/zip/<commit>` - not one
request per `GameSettings` file. The moving `master` reference is resolved
to an exact 40-character commit ID via the GitHub commits API first.

Measured against the real repository, the archive is on the order of tens of
megabytes even though it contains the whole working tree (source code and
all); the `Data/Sys/GameSettings` directory itself is under 2 MB across
roughly two thousand files. Every other archive entry's name and file mode
are still validated (path traversal, symlinks, device files, oversized
entries are all rejected), but only entries matching the exact
`<repo>-<commit>/Data/Sys/GameSettings/<GAMEID>.ini` shape are ever
decompressed - everything else is skipped without being read.

The download uses an identifiable EmuWiz User-Agent, a bounded overall
timeout, a bounded maximum download size, a manual redirect limit (only the
approved hosts are ever followed), retry with backoff on transient network
errors, and SHA-256 verification of the downloaded archive. The parsed
catalogue is written to disk only after the download, extraction, and
parsing all succeed - an interrupted or failed update leaves the previous
catalogue exactly as usable as it was before the attempt.

## Cache location

The catalogue lives entirely under EmuWiz's own cache
(`<EmuWiz data directory>/dolphin-cheat-catalogue/`), separate from:

- Dolphin's `User/GameSettings` directory
- transaction journals and backups
- installed cheat files
- ROM archives

Two files are written there, both atomically (temp file plus rename, with a
cross-process exclusive lock held for the duration of any write):

- `catalogue.json` - the active catalogue: metadata (resolved commit,
  download timestamp, archive SHA-256, parsed/usable/skipped counts,
  warnings) plus the compact indexed game records.
- `state.json` - the last update-check timestamp, updated independently of
  the (much larger) catalogue file so a quiet update check never rewrites
  the whole catalogue.

## Index and lookup

The index is keyed by exact six-character Game ID. Selecting a GameCube
game performs a binary search over the already-deserialized catalogue - no
re-scanning of any `.ini` file, no network request. Region mismatches and
malformed/ambiguous entries are blocked at lookup time; title similarity
never overrides an exact Game ID match.

## Update behaviour

The first download always requires an explicit click. After a catalogue
exists:

- it is used immediately, offline, on every game selection
- EmuWiz quietly checks upstream for a newer commit at most once per
  session, recording only the check timestamp and whether an update is
  available - this check never downloads the archive itself
- an available update is surfaced as an "Update available" state; the large
  download itself always still requires an explicit "Update catalogue" click
- "Check for updates" is also available manually at any time
- "Rebuild local index" re-parses the archive already pinned to the active
  commit (useful after an EmuWiz parser improvement) without checking
  upstream for a newer commit

## Offline use

Once downloaded, the catalogue answers every lookup without any network
access. If neither the catalogue nor a previously cached single-game result
has an entry for a selected game, EmuWiz does not automatically retry the
network - an explicit fetch remains available under Details.

## Removal

"Remove downloaded catalogue" requires confirmation and removes only
EmuWiz's own catalogue cache files. It never removes installed Dolphin
codes, never alters `User/GameSettings`, and leaves existing transaction
history untouched.

## Privacy and network behaviour

Requests carry no credentials, telemetry, or game-identifying information -
only the fixed GitHub API/codeload endpoints needed to resolve a commit and
download the pinned archive. No request is made without either an explicit
user click (download/update/rebuild) or the bounded, at-most-once-per-session
quiet update check.

## Malformed and unsupported entries

Malformed Gecko code bodies and ambiguous duplicate-named codes are blocked,
never silently repaired or deduplicated by guessing which one is "correct."
Non-Gecko settings in a `GameSettings` file are never reinterpreted as
cheats. Every skip and block is counted in the catalogue's statistics so the
numbers shown stay honest about what was actually found.
