# RetroArch External Cheat Catalogue Discovery and Matching

`archivefs retroarch-cheat-catalogue <local-path>` discovers which cheats a
**local** external catalogue source offers for your catalogued games, how
confidently each offer matches a game you own, whether it is already
installed, and - as of this milestone - exactly where it would go and what
would happen if it were staged. **No file is installed, copied, renamed, or
overwritten by this command, now or in any earlier milestone.** Every
`planned_action` in this document is a calculation only.

```console
archivefs retroarch-cheat-catalogue /path/to/cheat-catalogue
archivefs retroarch-cheat-catalogue /path/to/manifest.json --json
archivefs retroarch-cheat-catalogue /path/to/cheat-catalogue --cheat-destination-root /tmp/isolated-preview-root
```

The local catalogue path is always required and always exact. ArchiveFS
never searches your home directory, a default catalogue location, or any
remote source for one.

`--cheat-destination-root <path>` replaces the discovered RetroArch
environment's own cheat root for **staging-destination preview resolution
only** - isolated testing/preview use (e.g. previewing against a scratch
directory before RetroArch has even been installed, or reproducing a report
deterministically outside your real environment). It changes nothing else:
matching, installed-state resolution against an already-known
`retroarch-patch-preview` artifact-inventory destination, and catalogue
parsing are all unaffected. The path is never created, and omitting the flag
leaves every existing behavior exactly as it was before this flag existed.

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
- Exactly what path would a staging operation use, and what would it do
  there - install a new file, recognize it as already present, replace
  different content, or refuse as a conflict?
- Is it a safe candidate for future staging (advisory only - see
  "Staging preview" below; nothing is staged by this milestone)?

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
3. **Exact playlist identity** - the catalogue title/canonical-platform
   matches a game whose installed-artifact playlist evidence is itself `exact`
   (RetroArch's own playlist content path matched byte-for-byte).
4. **Exact normalized title + canonical platform + region.**
5. **Exact normalized title + canonical platform** (region ignored for the
   match itself, but see below). External platform hints are resolved through
   ArchiveFS's existing normalized folder-platform alias table; the original
   source string remains visible as provenance. Unknown or ambiguous hints do
   not participate in platform-gated tiers.
6. **Filename-only evidence** - normalized title alone, no platform
   corroboration.

Confidence levels, stable lower-snake-case JSON values:

- `exact`: tiers 1-3, exactly one candidate.
- `strong`: tier 4 or 5, exactly one candidate.
- `weak`: tier 6, exactly one candidate.
- `ambiguous`: two or more catalogue games tie at the best available tier -
  every tied candidate is listed; ArchiveFS never silently picks one.
- `unsupported`: no tier produced a candidate.

Region and revision differences remain visible rather than being silently
ignored: if tier 5 matches on title+platform alone but both sides also
declare a region (or a `(Rev N)`-style revision token), and they differ, an
extra `region_mismatch`/`revision_mismatch` evidence entry is attached to
the same match; it does not suppress the canonical title/platform match. A
revision token embedded in a title (e.g. `"Chrono Quest (Rev
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

As of this milestone, `unknown` is no longer the automatic result whenever
`retroarch-patch-preview`'s own artifact inventory has no expected
destination for the matched archive (e.g. no resolvable core). That case now
falls back to the same canonical-platform destination resolution the
staging preview below uses, so `installed_state` reflects a real probe/hash
result whenever a destination root (discovered or overridden) and a
resolvable platform are both available. `unknown` is still the honest result
whenever the environment/root genuinely cannot be determined, or the match
itself is `ambiguous`/`unsupported`.

## Staging preview

For every entry, `staging_plan` calculates the exact destination path and
what would happen there - still only a calculation; see the guarantee at
the top of this document. Fields:

- `source_cheat_path`: the catalogue source file this record came from.
- `proposed_destination_path`: `<destination root>/<canonical platform>/<game
  name>.cht`, or absent (`null`) when `planned_action` is `not_eligible` and
  no destination could even be computed.
- `source_file_hash` / `existing_destination_hash`: SHA-256 of the source
  file and, when the destination already exists as a readable regular file,
  of that file too.
- `planned_action`: one of the five stable lower-snake-case values below.
- `reason`: a stable, fixed identifier for why that action was chosen -
  never free-text prose.

### Destination resolution

The destination directory component is **only** ArchiveFS's own canonical
platform name for the catalogue record's platform hint (the same alias
table "Identity evidence and confidence" above uses for matching - e.g.
`"Atari - 2600"` resolves the same way to `"Atari2600"`). An unknown,
ambiguous, or unsafe platform hint is never used as a directory name, raw
or sanitized: if the hint does not resolve to a recognized canonical
platform, the result is `source_platform_unresolved`, exactly as if no
platform had been declared at all. Sanitizing a string only proves it is
*safe to use as a path component*; it never proves the string names a real,
trusted platform, so this command never treats "passed the sanitizer" as
license to use an unrecognized hint. This matters even when the match
itself came from serial/hash evidence that never looked at the platform
string at all - an exact match on serial does not make an attacker-supplied
`platform` field trustworthy.

The filename is the catalogue record's own game name plus `.cht`. Unlike
the platform component, the game name is not checked against any table (a
catalogue may legitimately name any game), so it is validated by
sanitization alone: rejected outright (`destination_traversal_rejected`) if
it is empty, `.`, `..`, or contains a path separator or NUL byte. The
already-canonical platform component is also run through the same
sanitizer as defense in depth, even though every table entry is already
safe by construction.

The destination *root* is, in order: `--cheat-destination-root` when given,
otherwise the first profile (in the environment's fixed native/Flatpak-user/
Flatpak-system order) with a usable, non-lossy discovered `cheats` path from
`retroarch-patch-preview`'s own environment discovery - not a new default
location this command invents. When `retroarch-patch-preview`'s existing
artifact inventory already has an expected destination for the matched
archive (a resolvable RetroArch core), that already-computed, core-based
path is used instead and takes precedence over the canonical-platform
resolver - it reflects the real, already-verified RetroArch runtime
convention exactly, and the canonical-platform resolver exists to cover
every case that path does not.

The destination root itself is never required to exist, and this command
never creates it, any subdirectory under it, or the destination file - a
missing root only ever changes whether `probe` reports `missing` (leading to
`install_new` when otherwise eligible), never whether the root gets created.

### Planned actions

Stable lower-snake-case JSON values:

- `install_new`: destination does not exist; match is `exact`/`strong`.
- `already_installed`: destination exists and its SHA-256 matches the
  source exactly.
- `replace_different`: destination exists with different content. **Preview
  only - nothing is overwritten.** Counts as a staging candidate, but is
  separately flagged `destructive_if_applied: true` on the entry so a
  future apply operation (not implemented by this or any milestone yet)
  cannot mistake it for a harmless new install.
- `conflict`: two or more source entries in this same report resolved to
  the identical destination path (`duplicate_destination`), or the
  destination itself could not be resolved safely - a symlink
  (`destination_symlink_not_followed`, never followed or read), an
  inaccessible or wrong-type path, more than one existing artifact-inventory
  candidate (`multiple_installed_candidates`), or a destination that could
  not be re-read for hashing.
- `not_eligible`: confidence below `strong` (`weak_match_not_eligible`,
  `ambiguous_match_not_eligible`, `unsupported_match_not_eligible`),
  incomplete catalogue parsing (`parsing_incomplete`), an absent, unknown,
  ambiguous, or otherwise unsafe (traversal-style, absolute, separator-
  containing) platform hint - anything that does not resolve to a
  recognized canonical platform (`source_platform_unresolved`), an unsafe
  game-name path component (`destination_traversal_rejected`), or no
  destination root/environment available at all
  (`destination_environment_unavailable`).

Duplicate-destination detection runs after every entry's own plan is
computed: any destination path named by more than one entry demotes *every*
entry naming it to `conflict`, even ones that would otherwise have been
`install_new` - a duplicate is never resolved by silently picking one
source over another.

`staging_candidate` is `true` exactly when `planned_action` is
`install_new`, `already_installed`, or `replace_different` - never for
`conflict` or `not_eligible`. Only `exact`/`strong` matches with complete
catalogue parsing can ever reach one of those three actions; a `weak` match
can never become a staging candidate, by construction, regardless of what
its destination would otherwise resolve to.

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

Each `entries[]` object additionally carries `staging_candidate` (bool),
`destructive_if_applied` (bool), and `staging_plan` (the object documented
under "Staging preview" above) alongside the pre-existing `game`,
`game_match`, `installed_state`, and `installed_state_detail` fields -
purely additive; no field present before this milestone was removed or
renamed. `summary` gained no new keys; `not_installed`, `already_installed`,
`conflicts`, and `staging_candidates` are now counted from every entry's
`staging_plan.planned_action` instead of `installed_state` alone, so a
`replace_different` entry now counts toward `staging_candidates` rather than
`conflicts` - the one intentional, documented behavior change to these
counts.

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
  the single expected destination);
- create the destination root, any platform subdirectory under it, or the
  destination file itself - `staging_plan` is a calculation, never acted on,
  regardless of `--cheat-destination-root`;
- follow a destination symlink, whether or not it escapes the destination
  root - a symlinked destination is always `conflict`, never read; or
- add an apply/install/stage operation of any kind. `staging_plan` answers
  "what would happen", never "make it happen" - no flag, override, or
  confidence level in this command performs an install, copy, or overwrite.

Any future download or staging *execution* workflow remains separately
gated by [`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md).
