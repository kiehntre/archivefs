# RetroArch External Cheat Catalogue Discovery and Matching

`archivefs retroarch-cheat-catalogue <local-path>` discovers which cheats a
**local** external catalogue source offers for your catalogued games, how
confidently each offer matches a game you own, and whether it is already
installed - without downloading, installing, enabling, applying, or changing
any cheat, and without any network access.

```console
archivefs retroarch-cheat-catalogue /path/to/cheat-catalogue
archivefs retroarch-cheat-catalogue /path/to/manifest.json --json
```

The local catalogue path is always required and always exact. ArchiveFS
never searches your home directory, a default catalogue location, or any
remote source for one.

## What this answers

For each game the local catalogue describes:

- Which cheats are available, and their bounded metadata (description,
  enabled-by-default state, declared index)?
- Which local source file supplied them, and its SHA-256?
- How strong is the match to a game in your catalogue?
- Which emulator/core family do they target, when the source declares one?
- Is a matching cheat file already installed, and does its content agree?
- Would installing it conflict with something already at the expected
  destination?
- Is it a safe candidate for future staging (advisory only - nothing is
  staged by this milestone)?

## Scope

Two local formats are supported:

- A directory tree of RetroArch/libretro `.cht` files (matched the same
  case-insensitive `.cht` extension rule as
  [`RETROARCH_ARTIFACT_INVENTORY.md`](RETROARCH_ARTIFACT_INVENTORY.md)). The
  immediate child directory of the catalogue root, if any, is offered as a
  platform hint for files nested under it; files directly under the root get
  no platform hint. A `.cht` file has no field for serial, content hash,
  region, or revision, so those matching tiers are unavailable from this
  format.
- A single bounded JSON manifest listing games and cheats - the only format
  able to declare a serial, content hash, region, or revision, and the
  format used by this project's own deterministic test fixtures.

Cheat *code* bodies (the numeric/hex value lines inside a `.cht` file) are
never parsed or stored by either format - only description text and
enabled-by-default state, mirroring
[`RETROARCH_ARTIFACT_INVENTORY.md`](RETROARCH_ARTIFACT_INVENTORY.md)'s
existing `CheatFileSummary` precedent. Human output never prints a raw cheat
code; JSON output never includes one.

Network-fetched catalogues, cheat-database indexes, or any remote source are
explicitly out of scope for this milestone. Adding one is a separately
reviewed future capability - see
[`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md).

## What this reuses instead of rebuilding

- Identity evidence for "is this catalogue game already in my library?"
  comes from the same read-only catalogue projection PCSX2 matching already
  consumes, loaded once through the existing, unmodified read-only database
  helper - this command never adds a database migration or write path.
- Playlist-identity evidence and installed-state both come from the same
  already-built `retroarch-patch-preview` advisory plan (its catalogue
  entries, playlist evidence, and existing-artifact inventory) rather than a
  second RetroArch discovery pass.
- Filesystem access reuses the same bounded, final-component-no-follow
  read-only filesystem trait `retroarch-patch-preview`'s artifact inventory
  uses.

## Identity evidence and confidence

Evidence is evaluated in this fixed order; the first tier with at least one
candidate decides the result. A namespace present on only one side is never
treated as a match or a conflict - it simply cannot decide that tier.

1. **Exact serial/product code** (JSON manifest only).
2. **Exact known content hash** (JSON manifest only) - reuses the same
   generic identity field PCSX2 matching already calls `executable_crc`.
3. **Exact playlist identity** - the catalogue title/platform matches a
   game whose installed-artifact playlist evidence is itself `exact`
   (RetroArch's own playlist content path matched byte-for-byte).
4. **Exact normalized title + platform + region.**
5. **Exact normalized title + platform** (region ignored for the match
   itself, but see below).
6. **Filename-only evidence** - normalized title alone, no platform
   corroboration.

Confidence levels, stable lower-snake-case JSON values:

- `exact`: tiers 1-3, exactly one candidate.
- `strong`: tier 4, exactly one candidate.
- `weak`: tier 5 or 6, exactly one candidate.
- `ambiguous`: two or more catalogue games tie at the best available tier -
  every tied candidate is listed; ArchiveFS never silently picks one.
- `unsupported`: no tier produced a candidate.

Region and revision differences remain visible rather than being silently
ignored: if tier 5 matches on title+platform alone but both sides also
declare a region (or a `(Rev N)`-style revision token), and they differ, an
extra `region_mismatch`/`revision_mismatch` evidence entry is attached to
the same match - it does not upgrade to `strong`, and it does not suppress
the match. A revision token embedded in a title (e.g. `"Chrono Quest (Rev
2)"`) is stripped before the title itself is compared, so a revision-only
difference does not by itself prevent tiers 5-6 from finding the title
match; the stripped token is still compared separately for the mismatch
note. A similarly named sequel (different trailing text, not a
parenthetical revision marker) is never treated as a match at any tier.

## Installed-state

Installed-state is evaluated only for a game matched with exactly one
candidate at `exact`, `strong`, or `weak` confidence, and only when an
advisory plan was supplied (the CLI command always supplies one; a caller of
the underlying library function may pass `None` to skip installed-state
entirely, e.g. `unknown`). Stable lower-snake-case JSON values:

- `not_installed`: the expected per-game cheat destination does not exist.
- `exact_file_present`: the expected destination exists and its SHA-256
  matches the catalogue source file's own hash exactly.
- `same_set_different_filename`: reserved for a future cross-file duplicate
  check; not yet populated by this milestone (see Non-goals).
- `destination_occupied_different_content`: the expected destination exists
  as a regular file whose content hash differs.
- `multiple_installed_candidates`: more than one existing artifact
  associates with the matched game - never resolved to one silently.
- `installed_file_malformed`: the installed file's own bounded cheat-file
  parse already reported diagnostics.
- `destination_symlink`: the expected destination's final path component is
  itself a symlink - never followed, never hashed.
- `inaccessible_destination`: the expected destination could not be read.
- `unknown`: no advisory plan was supplied, or the match was `ambiguous`/
  `unsupported`, or no installed-artifact association exists for the
  matched game at all.

Comparing installed content uses a fresh bounded read (the same 2 MiB limit
as a catalogue `.cht` file) of the expected destination through the same
no-write, no-follow filesystem trait used everywhere else in this command -
never a write, never an execute, never a second copy left on disk.

`staging_candidate` is advisory metadata only - `true` exactly when the
match is `exact`/`strong`, the catalogue record itself parsed without
diagnostics, and the installed state is `not_installed`,
`exact_file_present`, or `same_set_different_filename`. Nothing in this
command stages, copies, or installs a file because of this flag.

## Safety limits

Fixed in code:

- catalogue files scanned: 50,000;
- one `.cht` file or the JSON manifest body: 2 MiB / 8 MiB respectively;
- directories visited: 50,000;
- entries in one directory listing: 8,192;
- cheats per game record: 16,384;
- game records per catalogue: 100,000;
- diagnostics retained: 2,048 (truncation itself sets `complete: false`);
  and
- catalogue strings (names, platform, region, serial, descriptions): 4 KiB,
  rejected outright if they contain a NUL byte.

The catalogue root is probed and read the same no-follow way as every other
bounded filesystem operation in this codebase: a symlinked file is reported
as a diagnostic and never opened, and a symlinked subdirectory is never
traversed, so a catalogue root cannot be escaped by following a symlink to
content outside it. Oversized or malformed input is reported as a
structured diagnostic and skipped; it never panics and never silently
truncates a value as if it were complete.

## JSON contract

`format_version` is `1`, independent of the enclosing `retroarch-patch-preview`
plan's own format version when the CLI supplies one. Its exact top-level
keys are:

```text
format_version
read_only
complete
source_name
source_root
entries
summary
diagnostics
```

Enums use explicit lower-snake-case Serde names. Filesystem paths use the
existing `EncodedPath { display, lossy }` representation. Diagnostics use
stable `code`/`severity` fields, matching every other bounded discovery
command in this codebase.

## Non-goals

This command does not:

- access the network, or fetch, resolve, or cache anything from a remote
  cheat database;
- download, install, copy, rename, or delete a cheat file;
- enable, disable, or otherwise change any cheat's state;
- launch RetroArch, load a core, or execute any external command;
- modify RetroArch configuration, playlists, or cores;
- write to, migrate, or open (other than the existing read-only catalogue
  helper already used by sibling preview commands) the ArchiveFS database;
- run a live library scan;
- parse or execute a cheat *code* body, only its bounded description
  metadata;
- populate `same_set_different_filename` yet (planned: cross-referencing
  every installed cheat finding's hash against the matched game, not only
  the single expected destination); or
- add an apply/install/stage operation of any kind.

Any future download or staging workflow remains separately gated by
[`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md).
