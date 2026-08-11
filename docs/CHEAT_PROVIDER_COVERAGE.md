# Dolphin and RetroArch cheat-provider coverage

Status: implemented as a read-only, bounded audit on
`feature/cheat-provider-coverage-audit`. This document describes catalogue
coverage, not whether a particular code changes gameplay correctly.

## The important distinction

Provider discovery working means EmuWiz can retrieve or open an approved
source, validate its structure, match supported records, and safely preview or
apply an explicitly selected result. It does **not** mean that source contains
cheats for every game, region, revision, emulator core, or release.

Manual observations already completed by the user are deliberately narrow:

- Dolphin has discovered and successfully applied cheats for at least one
  game.
- RetroArch has discovered and successfully applied cheats for at least one
  game.
- Some Dolphin games appear to have limited or zero catalogue coverage.
- PCSX2 support is integrated, but no approved downloadable ordinary-cheat
  catalogue is bundled.

These observations prove those individual workflows. They are not universal
coverage or gameplay claims.

## Read-only coverage command

`emuwiz-cli cheat-provider-coverage` audits at most 32 exact persisted
archive IDs per run. It opens the library database read-only, bounded-inspects
only those selected game files for identity, and reads existing local provider
data. It does not scan the whole library and never installs, enables, copies,
renames, or edits anything.

```sh
emuwiz-cli library-list --json
emuwiz-cli cheat-provider-coverage \
  --id 12 --id 34 \
  --retroarch-catalogue /path/to/existing/cht \
  --json
```

Optional inputs:

- `--dolphin-cache-root <directory>` selects an existing local Dolphin
  catalogue cache; otherwise EmuWiz's normal cache location is read.
- `--retroarch-catalogue <directory-or-manifest>` selects an existing local
  `.cht` tree or bounded JSON manifest. Omitting it produces an honest
  `catalogue_unavailable` result for selected RetroArch games.
- `--json` emits the stable structured report; without it, the same facts are
  rendered as plain text.

The report contains titles, platforms, verified identity values, region,
revision, compatible and rejected counts, rejection categories, duplicates,
conflicts, unsupported formats, and a no-match reason. It intentionally omits
archive paths, catalogue paths, profile paths, tokens, credentials, and local
usernames. The command refuses PS2 and Xbox 360 selections because those route
to PCSX2 and Xenia rather than the two providers audited here.

## Dolphin provider

### Source and provenance

The approved source is the official `dolphin-emu/dolphin` repository's
`Data/Sys/GameSettings` dataset. EmuWiz resolves the moving upstream
revision to an exact commit, downloads the pinned repository archive with
bounded HTTPS handling, extracts only exact
`Data/Sys/GameSettings/<GAMEID>.ini` candidates into its own cache, records the
commit, archive digest, retrieval time, repository, attribution, and
GPL-2.0-or-later licence, then performs normal lookups offline.

An older, separately bounded exact-game fallback can retrieve one
`<GAMEID>.ini` and cache the validated result. Neither retrieval path writes a
Dolphin profile. Apply remains a later explicit shared transaction.

### Matching and identity

- Platform must route to Dolphin (GameCube or Wii).
- The game must have a verified six-character Game ID from the bounded disc
  header reader. Filename candidates cannot authorize lookup.
- Lookup is an exact, case-sensitive Game-ID index lookup.
- The fourth Game-ID character supplies the expected region; a different
  catalogue region is rejected.
- A code with explicit revision applicability is rejected when it differs
  from the verified disc revision.
- Current upstream GameSettings entries generally do not declare per-code
  revision applicability. EmuWiz labels that uncertainty rather than
  pretending every revision was verified.

Dolphin matching does not use serials, CRCs, generic filename similarity, or
RetroArch cores. The exact Game ID is the provider key.

### Formats, duplicates, and conflicts

The external provider parses and installs only `[Gecko]` definitions whose
body is one or more strict `XXXXXXXX YYYYYYYY` hexadecimal pairs. Missing
names, empty bodies, malformed lines, excess limits, and duplicate names are
not compatible coverage.

Local Dolphin inspection can identify `[ActionReplay]`, `[Gecko]`,
`[OnFrame]`, and enabled-name sections, but the approved external apply path is
Gecko-only. Action Replay and OnFrame content are not silently reclassified as
Gecko and do not increase the compatible count. Riivolution and texture/mod
content are unsupported.

Duplicate provider names are rejected instead of being counted as compatible.
Same-name definitions with different code bodies are also reported as
conflicts. During apply, an existing local Gecko definition with the same name
and different body blocks the merge; EmuWiz never overwrites it silently.
An upstream `enabled_by_default` marker remains metadata only—EmuWiz still
requires explicit user selection and confirmation.

### Proven coverage gaps

A Dolphin zero-match can mean:

- no local full catalogue is installed;
- the verified Game ID has no upstream GameSettings file;
- the file belongs to another encoded region;
- it contains no Gecko section or no structurally usable Gecko codes;
- all candidate codes were blocked as malformed, duplicate, conflicting, or
  revision-incompatible; or
- exact disc identity could not be verified.

The audit does not turn an absent exact Game-ID record into a fuzzy title
match. Catalogue breadth is therefore limited by the official upstream data,
not by weakening EmuWiz identity rules.

## RetroArch provider

### Source and provenance

The enabled approved source is the official `libretro/libretro-database`
repository, exposed as provider `libretro-buildbot-cheats`. EmuWiz resolves
`master` to an exact commit, downloads an immutable bounded archive from the
permitted GitHub host, validates paths/types/counts/sizes, retains only the
catalogue content described by its manifest, and records provider ID,
canonical repository, exact commit, archive digest, retrieval metadata,
provenance, and the upstream licence URL.

The coverage command consumes an already-existing local snapshot or local
fixture path. It never fetches automatically.

### Matching and core association

The existing RetroArch candidate matcher is reused. Its strongest available
evidence wins without promoting weaker evidence:

1. exact declared serial/product code;
2. exact declared content CRC/hash;
3. exact normalized title + canonical platform + region;
4. exact or article-normalized title + canonical platform;
5. title/filename-only evidence.

Real Libretro `.cht` files normally lack explicit serial, content hash,
region, and revision fields, so most matches depend on the catalogue's system
directory plus normalized game title. The bounded JSON format used by fixtures
can carry those stronger fields. A declared platform disagreement is a hard
cross-platform rejection. A different `target_emulator`, incomplete parsing,
or unsupported content is also rejected.

RetroArch playlist discovery can associate an exact content path with an
installed core and helps the separate destination-preview workflow. A `.cht`
record itself does not declare a libretro core, so core name is not used as a
provider-record identity key. The coverage report therefore describes a
core/emulator mismatch only when such contradictory record evidence exists;
it does not invent a core from an extension.

Strong and verified-exact candidates count as compatible. Filename/title-only
weak candidates and tied ambiguous candidates remain rejected in the coverage
report, even though another interactive UI may show them for explicit review.
This deliberately avoids increasing reported coverage by accepting uncertain
matches.

### Formats, duplicates, and conflicts

Supported source formats are:

- a bounded directory tree of case-insensitive `.cht` files; and
- a bounded JSON manifest used for structured providers and deterministic
  fixtures.

The `.cht` grammar supports indexed `cheatN_desc`, `cheatN_code`, and
`cheatN_enable` metadata under fixed entry/string/diagnostic limits. Malformed
UTF-8, malformed declarations, count inconsistencies, oversized content,
unsupported path encoding, unsupported content, symlinks, and resource-limit
truncation cannot become compatible entries. Soft patches (IPS/BPS/UPS/Xdelta),
Cheat Engine tables, encrypted formats, PNACH, Dolphin INI, and executable
content are not RetroArch `.cht` provider records.

Disabled entries are valid catalogue content but stay disabled until the user
explicitly selects and confirms them; disabled-by-default is never treated as
permission to auto-enable. Identical source-file hashes are counted as
duplicates. Multiple equally strong records become ambiguous, and the normal
availability/staging path also demotes records that resolve to the same
destination to a conflict instead of selecting one silently.

### Proven coverage gaps

A RetroArch zero-match can mean:

- no existing local catalogue was supplied;
- no source record has a related normalized title or exact identity;
- system-directory/platform evidence disagrees;
- only filename/title evidence exists and is too weak for coverage;
- several top candidates tie;
- the record targets another emulator/core context;
- the `.cht` file is malformed or unsupported; or
- a bounded scan was incomplete and the safety limit stopped evaluation.

Region and revision gaps are especially difficult to quantify for ordinary
Libretro `.cht` files because those fields are usually absent. Absence is
reported honestly; it is not treated as proof of compatibility.

## PCSX2 catalogue readiness

PCSX2 already has a provider-neutral catalogue model, verified executable-CRC
identity, optional serial/region constraints, strict plaintext PNACH parsing,
selection by stable record ID, safe staging, shared transactions, backups, and
rollback. It rejects unapproved providers, unverified records, duplicate IDs,
CRC/serial/region mismatches, malformed patch lines, encrypted formats, and
widescreen content presented as an ordinary cheat.

What is missing is an independently reviewed ordinary-cheat source with clear
ownership, licence, provenance, immutable retrieval, and record-verification
policy. The official `pcsx2_patches` repository primarily distributes
widescreen/no-interlace and related patches, not an approved ordinary-cheat
catalogue. EmuWiz therefore bundles no downloadable PCSX2 ordinary-cheat
provider and makes no PCSX2 gameplay-coverage claim.

## Future provider work

No additional provider is recommended for immediate integration by this
audit. A later provider-review milestone may evaluate candidates only after it
documents ownership, licence compatibility, stable exact identity keys,
region/revision semantics, duplicate/conflict rules, bounded parsing,
immutable retrieval, attribution, maintenance health, and representative
fixture coverage. Random PNACH collections, scraped code sites, fuzzy
title-only feeds, and sources without a clear redistribution licence are not
acceptable candidates.

Any future work should improve catalogue breadth while preserving the current
fail-closed match thresholds. An honest zero remains safer than a code for the
wrong game, region, revision, core, or executable.

## Automated and manual proof

Fixture tests cover exact Dolphin IDs, region/revision mismatch, Gecko versus
Action Replay classification, Dolphin duplicate/conflict blocking, exact
RetroArch content identity, platform/emulator mismatch, malformed and
unsupported records, ambiguity, coverage totals, rejection categories, JSON
shape, and input immutability. Tests use temporary or in-memory fixtures only.

Manual audit is intentionally bounded to four selected library entries: one
Dolphin match, one Dolphin limited/zero result, one RetroArch match, and one
RetroArch limited/zero result. It must not install or apply additional cheats.
The result records why each title matched or failed rather than extrapolating
from four games to the complete catalogues.
