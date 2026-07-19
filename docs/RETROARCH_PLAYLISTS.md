# RetroArch Playlist Identity and Content Matching

This milestone adds read-only discovery and parsing of RetroArch's own
`.lpl` playlist files, and uses them as additional, stronger identity
evidence for `archivefs retroarch-patch-preview`. It changes nothing
about how playlists themselves work: no playlist is ever created,
repaired, or modified, and RetroArch's own playlist-writing behavior is
untouched.

It builds on two earlier milestones: RetroArch environment discovery
(`retroarch-environment`, see [`RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md))
and the RetroArch cheat/patch destination preview
(`retroarch-patch-preview`, see [`RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md)).

## Why playlists

The prior patch-preview milestone matched a catalogue archive to an
installed core only through the archive's own outer file extension
(zip/7z/rar) compared against each installed core's declared
`supported_extensions`. This is weak for exactly the cases that matter
most:

- **Compressed content**: a core that only declares an uncompressed
  extension (e.g. `sfc`) never matches an ArchiveFS-managed `.zip`
  archive directly, even when RetroArch itself would happily load it.
- **Shared extensions**: several installed cores (e.g. multiple arcade
  cores, or several consoles that all read `.zip`) commonly share the
  same extension, producing `AmbiguousCore` even though the user has
  already told RetroArch, once, exactly which core loads which content -
  that information lives in their playlists.

RetroArch's own playlists already record, per piece of content, its
resolved path, which core loaded it, and (for scanned collections) a
database name and CRC32. This milestone reads that existing, trustworthy
record - never guessing, never writing it.

## Where this lives

Playlist **discovery and parsing** lives in
`archivefs-core::emulator_environment::retroarch` (the same module that
already discovers cores and configured paths), because it needs that
module's own private per-profile resolved-directory map to find the
already-discovered `Playlists` directory - reusing it directly rather
than re-deriving path resolution a second time. This is the *one*
intentional crossing of the "sibling, not part of" boundary between
`emulator_environment` and `patch_manager` (see
`RETROARCH_ENVIRONMENT.md`'s own updated note on this).

Playlist-to-catalogue **matching and core association** lives in
`archivefs-core::patch_manager::retroarch`, since it needs the read-only
ArchiveFS catalogue, which `emulator_environment` never touches.

## Verified playlist format

Every claim below was confirmed directly against the official
`libretro/RetroArch` source (`master` branch: `playlist.c`, `playlist.h`,
`libretro-common/file/file_path.c`), not guessed or taken from forum
posts.

### Classification

| Statement | Status |
| --- | --- |
| Modern playlists are JSON, top-level `version`/`items`, versions `"1.0"` and `"1.5"` both exist in the writer | Verified |
| The JSON reader never checks `version` at all - it is write-only metadata | Verified |
| A separate, non-JSON, config-file-style "old format" exists (`playlist.config.old_format`) | Verified, out of scope - see Non-goals |
| Entry fields: `path`, `label`, `core_path`, `core_name`, `crc32`, `db_name`, `entry_slot`, `subsystem_ident`, `subsystem_name`, `subsystem_roms` | Verified (main content-playlist writer) |
| `runtime_hours`/`runtime_minutes`/`runtime_seconds`/`last_played_*` also exist per-entry | Verified, but only in a separate runtime-log writer path (`playlist_write_runtime_file`), not the main content-playlist schema this milestone targets - not modeled here (see Non-goals) |
| `"DETECT"` is a real sentinel for "no specific core", not a name to look up (`FILE_PATH_DETECT`) | Verified |
| `#` splits an archive-member path *only* immediately after `.7z`, `.zip`, `.zst`, or `.apk` (case-insensitive) | Verified (`path_get_archive_delim`, `path_is_compressed_file`) |
| `.rar` is never treated as an archive container by RetroArch itself, so `#` after `.rar` is never a delimiter | Verified |
| `crc32` format is 8 uppercase hex digits plus a literal `|crc` suffix (e.g. `"A1B2C3D4|crc"`) | Verified (`tasks/task_database.c`, `manual_content_scan.c`) |
| `"00000000|crc"` is RetroArch's own "not computed" placeholder | Verified |
| Unknown/extra JSON fields are silently ignored on read | Verified |
| `content_history.lpl`/`content_favorites.lpl` are reserved special filenames with their own `db_name` fallback behavior | Verified (`playlist_get_db_name`), not specially modeled here (see Non-goals) |
| Duplicate `path` entries are possible in a hand-edited or externally-generated file (RetroArch's own write path de-duplicates, but the JSON grammar does not forbid it) | Verified |
| Playlist entry order is the JSON array's own order; `sort_mode` is a UI display preference, not a persisted reordering guarantee | Verified |
| A core's own filename/stem is its stable identity; `core_path` is installation-specific | Verified (`cheat_manager.c`'s `core_name = sysinfo.library_name`, matching the earlier cheat-preview milestone) |

### Unverified / out of scope

- Whether real-world RetroArch ever emits a UTF-8 BOM in a `.lpl` file was
  not confirmed either way; this parser strips one defensively regardless
  (the same policy `retroarch.cfg` parsing already uses).
- The binary `.rdb` content-database format is not investigated at all -
  not parsed in this milestone.
- Windows/macOS playlist locations are out of scope, matching
  `emulator_environment::retroarch`'s existing Linux-only scope.

## Bounds and safety

| Limit | Value |
| --- | --- |
| Maximum playlist file size | 4 MiB |
| Maximum playlist files scanned per profile | 1024 |
| Maximum entries parsed per playlist | 16384 |
| Maximum total entries across one profile's playlists | 65536 |

Exceeding any limit produces a structured diagnostic and marks the
affected playlist or profile `complete: false` - the entries collected so
far are still returned, never discarded wholesale. The byte-size cap is
what actually keeps parsing bounded: JSON has no separate declared-length
field to (mis)trust ahead of the bytes themselves, so a straightforward
bounded-read-then-parse approach is sufficient without a custom streaming
reader - the file is already capped at 4 MiB before `serde_json` ever
sees it, and the entry-count caps then bound what is *exposed*, not what
was *read*.

Only files ending exactly in `.lpl` are considered; subdirectories are
never scanned. A playlist directory that is a final-component symlink,
or a `.lpl` file that is itself a final-component symlink, is reported
via a diagnostic and never opened - the same no-follow policy
`emulator_environment::retroarch` already applies everywhere else.
Ancestor directories are not specially guarded against symlinks, exactly
as documented for the rest of that module.

No file is created, written, renamed, or deleted; no directory is
created; no process is spawned; no network call is made; no archive is
opened or extracted; no core is loaded; the binary `.rdb` database is
never parsed.

## Data model

`RetroArchProfile` gains a new field, `playlists: RetroArchPlaylistInventory`:

```
RetroArchPlaylistInventory {
    directory: Option<EncodedPath>,   // None if never resolved - never guessed
    playlists: Vec<RetroArchPlaylist>,
    diagnostics: Vec<Diagnostic>,
    complete: bool,
}

RetroArchPlaylist {
    file_path: EncodedPath,
    playlist_name: String,            // filename with `.lpl` stripped - a convenience label,
                                       // not a reproduction of playlist_get_db_name's own fallback
    version: Option<String>,          // informational only, never used to accept/reject
    default_core_path: Option<String>,
    default_core_name: Option<String>,
    entries: Vec<RetroArchPlaylistEntry>,
    diagnostics: Vec<Diagnostic>,
    complete: bool,
}

RetroArchPlaylistEntry {
    entry_index: u32,                 // zero-based, matches the JSON array's own index
    content_path: PlaylistContentPath,
    label: Option<String>,
    core_path: Option<String>,
    core_name: Option<String>,
    crc: PlaylistCrc,
    database_name: Option<String>,
    subsystem_ident: Option<String>,
    subsystem_name: Option<String>,
}
```

Playlist-internal string fields (`path`, `label`, `core_path`, ...) are
plain `String`, not `EncodedPath`: they came from already-UTF-8-validated
JSON text, so there is no lossy conversion to guard against. Only real
filesystem paths obtained via directory listing (`file_path`) use
`EncodedPath` - the same byte-safe, never-lossy-as-identity rule the rest
of this codebase already follows.

## Content-path model

```
ContentPathKind: Filesystem | ArchiveMember | Relative | Empty | Missing

PlaylistContentPath {
    raw: Option<String>,             // preserved exactly as written, for diagnostics/evidence
    kind: ContentPathKind,
    archive_path: Option<String>,    // Some only when kind == ArchiveMember
    archive_member_path: Option<String>,
}
```

`archive_path`/`archive_member_path` are only ever populated when the
verified `.7z`/`.zip`/`.zst`/`.apk`-then-`#` rule actually matches - a `#`
anywhere else (including after `.rar`) is left as a literal character.
Neither the archive nor its member is ever opened, read, or extracted;
this milestone only splits the *text* of the path.

## CRC model

```
PlaylistCrc: Verified { value } | Missing | Placeholder | Malformed { raw }
```

`Verified` requires exactly 8 hex digits followed by `|crc`; only the hex
digits are canonicalized to uppercase (a lossless case fold, never a
guess). `Placeholder` is the literal `"00000000|crc"`. Anything else
non-empty is `Malformed` and is reported, never coerced into looking
valid. **The CRC is never used as matching evidence** in this milestone:
ArchiveFS has no per-archive checksum field at all (`PersistedArchive`
carries none), and a playlist's CRC may describe an *inner* file inside
an archive rather than the outer archive ArchiveFS actually tracks -
comparing the two would silently conflate different logical objects. The
CRC is exposed only as informational evidence alongside whatever
`content_path`/`label` matching actually found.

## Database-name model

`database_name` is exactly the JSON `db_name` value when present and
non-empty - never RetroArch's own runtime fallback (playlist basename,
then a loaded core's declared databases; see `playlist_get_db_name`).
It is used only as *corroborating* context for a basename-only match
(promoting `Weak` to `Strong`) - it is never treated as proof of exact
game identity by itself, and it does not raise a match to `Exact`.

## Matching confidence model

Matching compares a playlist entry against every **present** catalogue
archive (rows already marked missing are excluded, mirroring the
existing PCSX2/RetroArch precedent), strongest evidence first:

1. **Exact** - the entry's own content path (non-archive) matches a
   catalogue archive's real path exactly, byte-for-byte.
2. **Strong** - an archive-member path's *outer* archive portion matches
   a catalogue archive's path exactly (the inner member is never
   verified - ArchiveFS has no inner-member identity to check against,
   so this deliberately never reaches `Exact`); or a normalized basename
   matches and the catalogue archive has a known platform (the only
   honestly available "corroborating evidence" without a `db_name`-to-
   platform mapping table, which this milestone does not build).
3. **Weak** - only a normalized basename or normalized label matched,
   with no corroborating platform evidence.
4. **Ambiguous** - two or more catalogue archives tied at the best
   available confidence; no single archive is chosen, and this can never
   be used to select a core (see below).
5. **Unsupported** - no usable evidence at all (no entry is produced).

A verified per-archive checksum tier (matching PCSX2's own currently-
unreachable `Exact` identity tier) is deliberately **not** implemented:
`PersistedArchive` has no checksum field to compare against, so it would
be structurally dead code.

## Core-association model

```
CoreAssociation:
  LinkedByCorePath { core_stem }    // core_path's own filename stem matches an installed core exactly
  LinkedByCoreName { core_stem }    // core_path was stale, but core_name matched exactly one installed core's display_name
  Detect                            // core_path and/or core_name is the literal "DETECT" sentinel
  AmbiguousCoreName { candidate_stems } // core_name matched 2+ installed cores sharing a display name
  NoInstalledCoreMatch              // present, not DETECT, but matches no installed core
  NoCoreEvidence                    // no usable core_path/core_name at all
```

A core's filename/stem (already inventoried by
`emulator_environment::retroarch`'s existing core discovery) is preferred
over its display name, since the stem is a stable identity while
`core_path` is installation-specific and `core_name` is a mutable display
string two different cores can share.

**Upgrade rule**: when extension-based matching alone left a disposition
of `AmbiguousCore` or `UnsupportedNoCore`, and every piece of `Strong`-or-
better playlist evidence for that archive in that profile agrees on
exactly one linked installed core, the disposition is upgraded to
`ExactCore` and `selected_core_source` records `PlaylistEvidence` (versus
`ExtensionMatch` for the pre-existing mechanism). An already-`ExactCore`
result from extension matching is **never** overridden by playlist
evidence, even if it disagrees - only ambiguous/no-match results can be
strengthened, never a working result weakened or second-guessed.

## Patch-preview integration and JSON compatibility

`RetroArchProfileOutcome` gains two additive fields:
`playlist_evidence: Vec<PlaylistEvidence>` and
`selected_core_source: Option<CoreSelectionSource>`. `RetroArchProfile`
(embedded via the environment report) gains `playlists`. Both are purely
additive - no existing field was removed, renamed, or changed in meaning.

**Format-version decision**: per this repository's documented JSON policy
(`docs/json-api.md`'s Stability Guarantees: "New fields may be added in
the future"), a purely additive field does not require a version bump.
`RetroArchEnvironmentReport.format_version` and
`RetroArchAdvisoryPlan.format_version` **both stay at `1`** - locked in by
`json_format_version_is_unchanged_by_additive_playlist_fields`. Existing
exact-key-set tests were updated (not weakened) to include the new keys,
exactly as this milestone's own instructions anticipated.

## CLI

No new top-level command was added. `archivefs retroarch-environment`
gains one concise summary line per profile ("Playlists found: N (M total
entries)"), not a full dump. `archivefs retroarch-patch-preview`'s human
output shows playlist evidence only when present (playlist name, entry
label, confidence, database name, core association) - it never dumps
every playlist entry by default. JSON output carries the full structured
evidence.

## Non-goals

- Writing, repairing, or creating any playlist.
- Parsing the legacy non-JSON "old format" playlist writer path.
- Modeling `runtime_hours`/`last_played_*` fields at all.
- Special-casing `content_history.lpl`/`content_favorites.lpl`.
- Parsing the binary `.rdb` content database.
- Opening, extracting, or reading inside any archive or archive member.
- Launching RetroArch, loading any core, or making any network call.
- A verified-checksum matching tier (no catalogue field exists to compare against).
