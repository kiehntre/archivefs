# RetroArch Cheat/Patch Destination Preview

`archivefs retroarch-patch-preview` is a strictly read-only command that
previews where a RetroArch cheat file or soft-patch sibling file *would*
go for each catalogued game, for each discovered RetroArch installation
profile. It makes no filesystem changes, launches no process, loads no
core, and makes no network call of any kind.

This is the second concrete preview built on top of `patch_manager`,
after PCSX2's `pcsx2-patch-preview`. It builds directly on the read-only
RetroArch environment discovery shipped earlier (see
[`RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md)) rather than
rediscovering any of the same paths, and it changes nothing about PCSX2:
no PCSX2 type, plan ID, JSON shape, or CLI output was touched to add this
command - see `docs/PATCH_CHEAT_MANAGER_DESIGN.md`'s "Emulator Adapter
Architecture" section for why RetroArch does not implement
`EmulatorAdapter` or produce an `AdvisoryPatchPlan`.

## Why this is not an `EmulatorAdapter`

`EmulatorAdapter` and `AdvisoryPatchPlan` are shaped narrowly around what
PCSX2's Phase 1 slice actually needed: one `data_root` per installation
candidate, one hypothetical relative path per metadata record fetched
from a single upstream source, and platform filtering hardcoded to
`"PS2"` in `patch_manager::mod`'s orchestration. None of that fits
RetroArch:

- RetroArch has no single patch/cheat root. It has several
  purpose-tagged directories per installation (cheats, cores, playlists,
  ...), already modeled by `emulator_environment::retroarch`'s
  twelve-purpose `PathFinding` list, not by a single `data_root`.
- RetroArch has a genuine **core-selection ambiguity** axis PCSX2 has no
  analogue for at all: its per-game cheat file path is scoped by which
  core loaded the content, and multiple installed cores can support the
  same file extension.
- There is no reviewed RetroArch metadata source. PCSX2 has one
  compiled-in, reviewed HTTPS endpoint (`patch_manager::BUILT_IN_SOURCE_URL`);
  no equivalent RetroArch source, licensing, or source policy has been
  reviewed for this milestone, so this preview makes **no network call
  of any kind**. `HttpsMetadataFetcher::fetch` continues to accept only
  the one PCSX2 URL and is not used, imported, or extended here.

Forcing RetroArch through the existing trait would either weaken it for
PCSX2 or silently misrepresent RetroArch. Instead, `patch_manager::retroarch`
is a narrowly-scoped, independent module with its own
`RetroArchAdvisoryPlan` type, exactly as the design review anticipated
("If RetroArch needs a separate advisory type, explain why and keep it
narrowly scoped").

## What "matching" means here

Because there is no external metadata record for RetroArch in this
milestone, there is also no PCSX2-style "which catalogue game does this
record belong to" problem - a catalogue row is never ambiguous with
*itself*. Every advisory entry is produced from exactly two already-local
inputs:

1. The already-discovered `RetroArchEnvironmentReport` (profiles, each
   profile's resolved `Cheats` path, and its installed cores' `.info`
   metadata - `supported_extensions` in particular).
2. The read-only ArchiveFS catalogue (every **present** archive - rows
   already marked missing are excluded, matching PCSX2's own treatment).

The one genuine ambiguity this preview resolves is **core selection**:
which installed core, if any, would load this archive. That is decided
by comparing the archive's own file extension (ArchiveFS tracks Zip/
SevenZip/Rar archives; this is the archive's own container extension,
not an inner compressed entry's extension - see "Non-goals" below)
against every installed core's declared `supported_extensions`,
case-insensitively:

- **Exactly one** installed core supports the extension -> `ExactCore`,
  and a per-game cheat file destination is proposed
  (`selected_core_source: extension_match`).
- **Two or more** installed cores support the extension -> `AmbiguousCore`,
  and no single destination is proposed - every tied core stem is listed
  as a candidate, but nothing is elevated to an executable-looking
  action - *unless* a later milestone's playlist evidence unambiguously
  links exactly one of those installed cores, in which case this is
  upgraded to `ExactCore` with `selected_core_source: playlist_evidence`.
  See [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md).
- **Zero** installed cores support the extension -> `UnsupportedNoCore`,
  similarly upgradable by unambiguous playlist evidence.
- The archive has no usable (or non-UTF-8) file extension ->
  `UnsupportedNoContentExtension`.
- This profile's `cheat_database_path` is not configured, is left
  unresolved (a colon-alias or plain relative value - see
  `RETROARCH_ENVIRONMENT.md`'s "Path resolution"), or was never set ->
  `UnsupportedCheatsPathUnresolved`.
- This profile's `cheat_database_path` resolved to a real path, but that
  path does not currently exist as a directory ->
  `UnsupportedCheatsPathMissing`.

**No `serial`/`executable_crc`-style exact identity tier is used at
all**, not even opportunistically. Unlike PCSX2 (which at least has an
upstream record to compare those fields against, even though the
catalogue side is unpopulated in production today), RetroArch has no
second record here to compare a catalogue row's identity fields against
in this milestone - a same-row "exact match" would be vacuous. This is a
deliberate scope decision, not an oversight; the
`no_identity_tier_is_used_for_retroarch_matching` test locks it in.

A later milestone added a genuinely different second source of evidence:
RetroArch's own `.lpl` playlists, which *do* record a resolved content
path, core association, and (for scanned collections) a database name
for content the user has already loaded. That evidence is read-only,
never invents anything ArchiveFS doesn't already know, and is documented
separately in [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) rather
than here, since it introduces its own confidence vocabulary
(`exact`/`strong`/`weak`/`ambiguous`/`unsupported`) distinct from the
extension-based `CoreMatchDisposition` above.

Region, revision, and disc-number evidence have **no bearing** on
RetroArch's own core/cheat-destination selection - verified from the
source cited below: the destination is keyed only by which core loads
the content and the content's own current basename, never by title,
region, or revision metadata. This is a genuine, verified finding, not a
missing feature.

## Destination kinds

Three distinct destination kinds are modeled, never flattened into one:

| Kind | Where it lives | Scope |
| --- | --- | --- |
| `cheat_database_root` | `<cheat_database_path>` itself | Per profile |
| `per_game_cheat_file` | `<cheat_database_path>/<core_library_name>/<content_basename>.cht` | Per (profile, matched core) |
| `soft_patch_sibling` | `<content-dir>/<content-basename-without-extension>.{ips,bps,ups,xdelta}` | Per archive, **profile-independent** |

A soft-patch sibling destination lives next to the archive itself, not
under any RetroArch-owned directory at all, so it is computed once per
catalogue archive regardless of how many RetroArch profiles are
installed.

Every proposed destination reports: its `kind`; a byte-safe `path` and
`file_name` (`EncodedPath` - a lossy display string plus an honest
`lossy` flag, never a plain `String` used as identity); a stable
`derivation` code; whether its parent directory exists; whether the
destination itself already exists; a `conflict` flag (`true` exactly
when the destination already exists - advisory only, nothing here ever
reads, replaces, or removes it); and, when unsupported, a stable
`unsupported_reason` code.

## Verified RetroArch behaviour

Every claim below was confirmed directly against the official
`libretro/RetroArch` source (`master` branch, files `cheat_manager.c`,
`runloop.c`, `tasks/task_patch.c`, `tasks/task_content.c`, `playlist.c`),
not guessed or taken from forum posts:

- **Cheat file format** (verified): RetroArch's own config-file-style
  text format, `.cht` extension - `cheats = N` plus `cheatN_desc`/
  `cheatN_code`/`cheatN_enable`/`cheatN_handler`/... keys per entry
  (`cheat_manager_save`/`cheat_manager_load`). This preview never parses
  cheat code content - only the destination *path* is previewed, which
  is safe without understanding cheat syntax at all.
- **Per-game cheat file destination** (verified):
  `<cheat_database_path>/<core_library_name>/<content_basename>.cht`,
  where `core_library_name` is the loaded core's own
  `retro_system_info.library_name` and `content_basename` is the
  content's own basename with its extension replaced by `.cht`
  (`cheat_manager_get_game_specific_filename`). Association is by core
  identity plus content filename - **not** by CRC, serial, or database
  identity.
- **Soft-patch formats** (verified): IPS, BPS, UPS, and Xdelta are all
  natively supported (`tasks/task_patch.c`: `try_ips_patch`,
  `try_bps_patch`, `try_ups_patch`, `try_xdelta_patch`).
- **Soft-patch sibling filename convention** (verified):
  `<content-basename-without-extension>.{ups,bps,ips,xdelta}`, in the
  content's own directory (`runloop.c`'s `runloop_path_fill_names`).
  This applies even to compressed/archived content read via
  `file_archive_compressed_read` - the sibling name is still derived from
  the archive's own path, not an inner entry name
  (`content_file_load_into_memory`, `runloop_path_set_basename`'s
  `HAVE_COMPRESSION` branch) - which is exactly why an ArchiveFS-managed
  archive's own path is a correct, direct basis for this destination.
- **Soft-patch precedence** (verified): when no per-content core option
  overrides it, RetroArch tries IPS, then BPS, then UPS, then Xdelta,
  applying the first one found (`tasks/task_patch.c`'s `patch_content`).
  Only one of the four may be explicitly preferred at a time; declaring
  more than one preference aborts patching entirely.
- **Multiple/chained patches** (verified): after the first (non-indexed)
  patch is applied, RetroArch looks for additional *indexed* patches
  (`<name>.<ext>1` .. `<name>.<ext>9`, single digit only), trying all
  four extensions at each index in ascending order and stopping at the
  first missing index. This preview does not model indexed destinations
  (see "Non-goals").
- **Playlist identity fields** (verified, not used): `.lpl` playlist
  entries carry `path`, `label`, `core_path`, `core_name`, `crc32`,
  `db_name`, and (for subsystems) `subsystem_ident`/`subsystem_name`/
  `subsystem_roms` (`playlist.c`). These *are* useful identity evidence
  in principle, but parsing playlists is out of scope for this
  milestone - see "Non-goals".

### Classification

| Behaviour | Status |
| --- | --- |
| `.cht` file format and per-game filename derivation | Verified |
| Soft-patch sibling naming and try-order | Verified |
| Chained/indexed patch application | Verified, not modeled as a destination |
| Playlist identity fields exist | Verified, not parsed |
| Cheat code *content* semantics (handler, memory search size, ...) | Core-specific; irrelevant here since no cheat code content is parsed |
| RDB (content database) binary format | Unknown; not investigated, out of scope |
| A distro-provided shared cheat database outside `cheat_database_path` | Unknown; not investigated |
| Windows/macOS discovery | Out of scope - this milestone is Linux-only, matching `emulator_environment::retroarch`'s existing scope |

## Read-only guarantees

No file is created, written, renamed, or deleted; no directory is
created; no process is spawned; no core is loaded; no network call is
made. All filesystem access is read-only existence probing through the
same `ReadOnlyHostFilesystem` trait `emulator_environment::retroarch`
already uses (final-component symlinks are never followed). These
guarantees are enforced by `preview_makes_no_filesystem_writes_and_no_migration`
in `patch_manager::retroarch`'s own test module, which compares the exact
filesystem tree and catalogue bytes before and after a preview run,
exactly mirroring the equivalent PCSX2 fixture test.

## CLI

```
archivefs retroarch-patch-preview
archivefs retroarch-patch-preview --json
```

Only `--json` is accepted, matching `pcsx2-patch-preview`'s existing
convention. Human output states, up front: that the preview is advisory
only and non-executable, the plan ID, and an explicit "no files changed,
no core loaded, no network call" line. It exits `0` whenever discovery
and preview complete, including when nothing matches or no RetroArch
installation is found at all; a non-zero exit means a genuine command
failure (an unreadable catalogue, an unexpected argument, or `HOME` being
unset).

## JSON contract

```json
{
  "format_version": 1,
  "plan_id": "...",
  "executable": false,
  "environment": { /* the same RetroArchEnvironmentReport shape documented in RETROARCH_ENVIRONMENT.md */ },
  "entries": [
    {
      "archive_id": 7,
      "display_name": "...",
      "normalized_name": "...",
      "platform": "SNES",
      "content_extension": "zip",
      "soft_patch_candidates": [ /* 4 ProposedDestination, ips/bps/ups/xdelta order */ ],
      "profile_outcomes": [ /* exactly 3, native/flatpak-user/flatpak-system order */ ]
    }
  ],
  "summary": {
    "catalogue_archives": 1,
    "exact_core_profile_outcomes": 1,
    "ambiguous_core_profile_outcomes": 0,
    "unsupported_profile_outcomes": 0
  }
}
```

- All enums serialize as stable `lower_snake_case` strings
  (`disposition: "exact_core"`, `kind: "per_game_cheat_file"`, ...).
- Every path is an `EncodedPath` object (`{"display": "...", "lossy": bool}`),
  never a plain string used as identity - see `RETROARCH_ENVIRONMENT.md`'s
  own JSON contract section, which this reuses unchanged for the embedded
  `environment` field.
- `derivation` and `unsupported_reason` are drawn from a small, fixed set
  of stable identifier-like strings, never free-text prose.
- `entries[]` is sorted by `archive_id`; `profile_outcomes[]` follows the
  same fixed native/Flatpak-user/Flatpak-system order as
  `environment.profiles[]`; `soft_patch_candidates[]` is always in
  RetroArch's own ips/bps/ups/xdelta try-order. None of this ordering
  depends on filesystem enumeration order, confirmed by
  `plan_entries_are_sorted_by_archive_id_regardless_of_input_order` and
  `plan_id_is_stable_regardless_of_input_archive_order`.
- Exact JSON key sets for a destination object are locked by
  `json_destination_key_set_is_stable`.
- No PCSX2 field name is reused where its meaning differs - this
  contract shares no type with `AdvisoryPatchPlan`.
- Each `profile_outcomes[]` entry gained two additive fields:
  `playlist_evidence` (an array, possibly empty, of matched playlist
  entries naming this archive) and `selected_core_source`
  (`"extension_match"` or `"playlist_evidence"`, `null` when no core was
  selected). `format_version` stays `1` - see
  [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) for the full
  `playlist_evidence` field shape and the format-version decision.

## Current limitations

- Matching is possible only when at least one RetroArch core is actually
  installed and its `.info` metadata is readable; a fresh RetroArch
  install with no cores yields `UnsupportedNoCore` for every archive.
- The archive's *own* extension (zip/7z/rar) is compared against
  `supported_extensions` - not any extension an archive's *inner*
  compressed content might have. A core that only declares an
  uncompressed extension (e.g. `sfc`, not `zip`) will never match an
  ArchiveFS-managed `.zip` archive directly through this preview, even if
  RetroArch could load the ROM after ArchiveFS mounts it.
- Exact identity evidence (a per-archive checksum ArchiveFS itself holds)
  is never used - see "Why this is not an `EmulatorAdapter`" and the
  honesty note above. RetroArch's own playlists are now read as a
  *different* kind of evidence (see [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md)),
  but a playlist's CRC is never compared against anything ArchiveFS has,
  since it may describe an inner archive member ArchiveFS does not track.
- The binary `.rdb` content database is still not parsed.

## Core-specific caveats

- Which core a real user would actually pick when several are installed
  and extension-compatible is now resolved when the user's own RetroArch
  playlists unambiguously say so (see
  [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md)); without that
  evidence, or when it is itself ambiguous or missing, it is still
  reported as `AmbiguousCore`, never guessed.
- Arcade content (MAME, FBNeo, ...) participates in the same
  extension-matching path as any other core; no arcade-specific
  shortname/DAT verification is performed.

## Non-goals

Everything a mutation-capable manager would eventually need is out of
scope here: writing, enabling, or installing any cheat or patch file;
downloading a cheat database or patch content from any source; writing,
repairing, or creating any `.lpl` playlist, or parsing the binary `.rdb`
content database (`.lpl` playlists are now *read*, as evidence - see
[`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) - but never written);
modeling indexed (chained) patch destinations beyond the first; launching
RetroArch or any core; and any network call. This command only previews
destinations for content ArchiveFS already knows about, using data
already on disk.
