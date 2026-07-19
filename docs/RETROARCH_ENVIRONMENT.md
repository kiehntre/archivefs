# RetroArch Environment Discovery

The same final-component no-follow, bounded-read, bounded-listing filesystem
abstraction is reused by the existing cheat/patch artifact inventory; see
[`RETROARCH_ARTIFACT_INVENTORY.md`](RETROARCH_ARTIFACT_INVENTORY.md). Environment
discovery itself remains concerned only with profiles, configured paths, cores,
playlists, and AppImage evidence.

`archivefs retroarch-environment` is a strictly read-only command that
discovers and reports the local RetroArch environment: which installation
profiles exist, where their configuration lives, which of a fixed set of
configured directories resolve and exist, and which cores (and core
metadata) are installed. It makes no filesystem changes, launches no
process, makes no network call, and loads no core.

This was originally built as a **sibling** to the PCSX2 patch-preview
adapter boundary (`archivefs-core::patch_manager`), not part of it - see
[`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md#emulator-adapter-architecture)
for why environment discovery and patch preview were originally kept as
independent systems. No `patch_manager` code, type, JSON output, or
historical plan ID was changed to add this feature, and none of that
remains true today either: this module's own public API, behavior, and
output are completely unchanged.

What has changed since: `patch_manager::retroarch` (the
`retroarch-patch-preview` command; see
[`RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md)) now reuses
`discover_retroarch_environment` and this module's `ReadOnlyHostFilesystem`
trait directly, rather than rediscovering the same paths a second time.
That is a one-directional dependency: `patch_manager` now depends on
`emulator_environment`, but nothing in this module imports from
`patch_manager`, and this module's own report shape, JSON contract, and
CLI output are exactly what they were before that dependency existed.

A later milestone added read-only detection of RetroArch installed as an
AppImage - see [`RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md) for the
full design record. In the common case (an AppImage shares the native
profile's own configuration) this is purely additive: a new `app_images`
field on the existing native profile. Only when an AppImage has verified
evidence of a genuinely distinct configuration directory does a fourth
profile appear - see "What is discovered" and the JSON contract below.

## What is discovered

For native, Flatpak (user scope), and Flatpak (system scope) - always
reported in that relative order, even when nothing is found for a given
profile - plus, when a distinct-configuration AppImage was found, one
additional AppImage profile inserted between native and Flatpak (user):

- **Evidence**: native executables found on `PATH` (deterministic order,
  deduplicated), whether the Flatpak app directory exists for that scope,
  whether the config directory exists, whether `retroarch.cfg` exists.
- **Config directory and file**: their resolved location, filesystem
  classification, and (for the config file) its parse outcome.
- **Twelve configured paths**: System, Cores, CoreInfo, Saves, SaveStates,
  Playlists, Shaders, Overlays, Thumbnails, JoypadAutoconfig, Database,
  Cheats - each reported as configured/resolved/existing, configured but
  unresolved, "runtime default unknown," or unreadable, never conflated.
- **Installed cores**: every `*_libretro.so` file in the resolved Cores
  directory, sorted by raw filename bytes, each with its optional `.info`
  metadata (`display_name`, `display_version`, `systemname`,
  `supported_extensions`) when present and readable.
- **Playlist inventory**: every `*.lpl` file in the resolved Playlists
  directory (sorted by encoded playlist path bytes, non-recursive), each
  parsed into its declared entries (content path, label, core
  association, CRC, database name, subsystem fields) subject to fixed
  bounds - see [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) for the
  full field/bounds/diagnostics record. Added by a later milestone; the
  rest of this document's own scope is otherwise unchanged.
- **AppImage candidates** (native and, when it exists, the distinct
  AppImage profile only - always empty for both Flatpak profiles): every
  detected AppImage with real evidence of being RetroArch, its
  identification confidence, executable state, and configuration
  association - see [`RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md) for
  the full record.
- **Diagnostics**: structured, machine-readable findings (a relative path
  that couldn't be resolved, a configured directory that's missing, an
  `#include` that wasn't followed, an oversized file, ...), sorted
  deterministically, never free-text.

## What is not discovered (v1 scope)

Only twelve path purposes are modeled. Deliberately **not** included in
this milestone: `assets_directory`, audio/video filter directories,
`input_remapping_directory`, `recording_output_directory`, `log_dir`,
`cache_directory`, `screenshot_directory`, `content_history_path`, and
`content_favorites_path`. These can be added later without breaking the
existing output shape (new path purposes are appended to the end of the
fixed order, not inserted).

Also out of scope, deliberately:

- RetroArch's own version (no filesystem-only, execution-free signal was
  established for it; adding this later must not spawn `retroarch
  --version` or any other process).
- Any "default path" RetroArch itself would apply when a key is empty or
  missing - see [Path resolution](#path-resolution) below.
- More than one native or Flatpak profile (e.g. a portable install) - a
  future `--config <path>` override is a natural, separately-scoped
  addition. (AppImage installations are the one exception: at most one
  additional, distinct AppImage profile is now supported - see
  [`RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md).)
- Reading, following, or merging `#include`d config fragments.

## Native and Flatpak behaviour

**Native**: the config directory is `$XDG_CONFIG_HOME/retroarch`, falling
back to `$HOME/.config/retroarch` when `XDG_CONFIG_HOME` is unset, empty,
or - per the XDG Base Directory Specification - relative (a relative
`XDG_CONFIG_HOME` is ignored, not resolved against any implicit base).
Native executables are discovered by searching `PATH` for a regular,
executable file literally named `retroarch`; this deliberately *does*
follow symlinks (unlike everything else in this module), since many real
`retroarch` binaries on disk are symlinks (e.g. via `update-alternatives`)
and PATH lookup conventionally follows them.

**Flatpak**: the official app ID is `org.libretro.RetroArch` (confirmed
against the official Flathub manifest,
`github.com/flathub/org.libretro.RetroArch`). "Installed" evidence for
each scope is the existence of `<flatpak-root>/app/org.libretro.RetroArch`
under the user root (`~/.local/share/flatpak` by default) or the system
root (`/var/lib/flatpak` by default) - this is evidence the app is
*installed*, not that it has ever been launched or has a config file
(tracked separately). Flatpak's own environment setup - distinct from
generic XDG defaulting - sets `XDG_CONFIG_HOME` inside the sandbox to
`$HOME/.var/app/org.libretro.RetroArch/config` (no leading dot before
`config`), so the config file is
`~/.var/app/org.libretro.RetroArch/config/retroarch/retroarch.cfg`
regardless of whether the app was installed at user or system scope (per-app
data always lives under the invoking user's home). A tilde-prefixed value
inside a Flatpak profile's config resolves against the Flatpak sandbox's
own home (`~/.var/app/org.libretro.RetroArch`), not the host's real
`$HOME` - resolving it against the host's `$HOME` would be wrong, since
that is not what the RetroArch process running inside the sandbox would
have seen.

## Path resolution

Every claim below is confirmed against the official RetroArch source
(`libretro/RetroArch`, files `configuration.c`, `file/config_file.c`,
`file/file_path.c`, and `frontend/drivers/platform_unix.c`), not guessed:

- **Absolute** (`/...`): used directly.
- **Tilde** (`~/...` or exactly `~`): expands against the profile's own
  home (host `$HOME` for native, the Flatpak sandbox home for Flatpak) -
  confirmed applied to every path-typed setting by `config_get_path` via
  `fill_pathname_expand_special`.
- **Colon-prefixed** (`:...`, RetroArch's own application-directory
  alias): reported as configured but **unresolved**. Resolving it would
  require knowing RetroArch's own install directory, which this
  read-only, no-execution milestone cannot determine.
- **Plain relative** (no `/`, `~`, or `:` prefix): reported as configured
  but **unresolved**. Confirmed from source that RetroArch itself does
  *not* anchor such a value to the config file's directory or to any XDG
  base at config-read time (`fill_pathname_expand_special` returns it
  unchanged); ArchiveFS does not invent a resolution base RetroArch itself
  doesn't use.
- **Empty value** (`""`): a real, distinct configured state - "runtime
  default unknown," never "not configured."
- **Missing key**: also "runtime default unknown," never described as
  "not configured" - RetroArch applies its own runtime default in both
  cases, which this milestone does not attempt to reproduce. Several of
  the needed defaults (System, Cores, CoreInfo, Database, Shaders,
  JoypadAutoconfig) are gated behind compile-time/package-specific values
  ArchiveFS cannot observe from outside the RetroArch binary, so no
  default is guessed for *any* purpose, keeping the policy uniform and
  honest rather than confidently wrong for some users.

## Config file grammar

`retroarch.cfg` and `.info` files (which use the identical grammar) are
parsed with: optional UTF-8 BOM, LF and CRLF line endings, `#`
whole-line comments and directives, trailing `#` comments (never inside a
quoted value), quoted and unquoted values (an unquoted value truncates at
the first whitespace character - matching real RetroArch behaviour
exactly, not a parser gap), empty quoted values, and **first-occurrence
wins** on duplicate keys (confirmed directly from `config_file.c`'s own
comment - the opposite of the common "last wins" INI convention). A
non-`key = value`, non-comment, non-blank line is reported as malformed by
one-based line number; parsing continues past it. `#include "..."`
directives are detected but never followed; when one is present, the
config read is marked `complete: false` and a diagnostic is emitted.

## Symlink policy

Final-component symlinks are **never followed** for config files,
configured directories, core files, or `.info` files - `symlink_metadata`
is used, and a symlinked final component is reported as `symlink` without
opening it. Native executable discovery via `PATH` is the one deliberate
exception (see above). Ancestor directories are *not* specially guarded
against symlinks; the operating system resolves them normally, matching
every other filesystem-reading command in this codebase. There is an
inherent, unavoidable gap between a probe and a subsequent bounded read on
POSIX filesystems without `openat2`/`RESOLVE_NO_SYMLINKS`, which this
milestone does not use; the practical exposure is small since anything
read is bounded, never executed, and never trusted beyond parsing a
handful of known text fields.

## Read-only guarantees

No file is created, written, renamed, or deleted; no directory is
created; no process is spawned; no network call is made; no core is
loaded or executed. Reads are bounded: `retroarch.cfg` at 2 MiB, `.info`
files at 128 KiB, and directory listings at 4096 entries - exceeding a
limit produces a structured diagnostic, never a partially-trusted read.
These guarantees are enforced by fixture tests (`discovery_makes_no_filesystem_writes`
in `emulator_environment::retroarch::tests`) that compare the exact
filesystem tree before and after a discovery run.

## JSON contract

```
archivefs retroarch-environment --json
```

Top level:

```json
{
  "format_version": 2,
  "profiles": [ /* 3 or 4 - see "Profile ordering" below */ ],
  "diagnostics": [ /* report-level only, e.g. a relative XDG_CONFIG_HOME */ ]
}
```

### Profile ordering

`profiles[]` is 3 entries (native, Flatpak/user, Flatpak/system, in that
order) unless a distinct-configuration AppImage was found, in which case
it is 4 entries with the new AppImage profile inserted **between** native
and Flatpak/user: native, AppImage, Flatpak/user, Flatpak/system. This is
`ProfileKind`'s own derived `Ord` (`Native < AppImage < Flatpak`), which
profile sorting already relies on. See
[`RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md) for exactly when the
4th profile appears versus when an AppImage is instead folded into the
native profile's own `app_images` field.

- All enums serialize as stable `lower_snake_case` strings.
- Enums that carry data (`ConfigReadOutcome`, `CoreInfoFinding`) use an
  internally-tagged `{"type": "...", ...}` shape (`#[serde(tag = "type",
  rename_all = "snake_case")]`).
- Paths never fail to serialize, even for a non-UTF-8 Unix path: every
  path is an `EncodedPath` object (`{"display": "...", "lossy": bool}`),
  a lossy display string plus an honest flag rather than a
  `serde_json` error.
- Diagnostics are structured (`code`, `severity`, `detail_kind`,
  `profile`, `purpose`, `path`, `entry_index`) - no free-text `message`
  field belongs in the stable contract; human wording lives only in the
  CLI formatter. `entry_index` is additive (added for playlist entry-level
  findings) and is `null` for every pre-existing, non-entry-specific
  diagnostic.
- `paths[]` is in a fixed declared order (System, Cores, CoreInfo, Saves,
  SaveStates, Playlists, Shaders, Overlays, Thumbnails, JoypadAutoconfig,
  Database, Cheats); `cores[]` is sorted by raw filename bytes;
  `playlists.playlists[]` is sorted by encoded playlist path bytes, and
  each playlist's own `entries[]` preserves the source JSON array's own
  order (`entry_index` is the zero-based index into that same array);
  `diagnostics[]` is sorted by severity, then code, then profile, then
  purpose, then path, then entry index. None of this ordering depends on
  filesystem enumeration order.
- Exact key sets for the report, each profile, each path finding, each
  core, and the `playlists`/playlist/entry/content-path shapes are
  locked by regression tests (`json_report_key_sets_are_stable`) - see
  [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) for the full
  playlist field documentation, and [`RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md)
  for the `app_images[]` field shape added to each profile.
- `format_version` is `2` (bumped from `1`): unlike the playlist
  milestone's purely additive fields, `profiles[]` can now have a 4th
  element inserted *between* native and Flatpak/user, shifting what index
  2 (Flatpak/system) means for any consumer that indexed into this array
  positionally rather than by `profile_kind`. Per this project's
  documented JSON policy (`docs/json-api.md`), purely additive object
  fields never require a version bump, but a positional array-shape
  change like this one does.

There is no `report_id` or snapshot fingerprint in this milestone.

## CLI

```
archivefs retroarch-environment
archivefs retroarch-environment --json
```

No other flags exist (`--native`, `--flatpak`, `--config`, `--profile`,
and `--verbose` were deliberately not added - see the design review that
produced this milestone). The command exits `0` whenever discovery
completes, including when RetroArch is not found at all; a non-zero exit
means an invalid argument, or that `HOME` could not be determined (the
only condition under which no discovery roots exist at all).

## Non-goals

Everything a mutation-capable emulator manager would eventually need is
out of scope here: editing `retroarch.cfg`, installing/updating/removing
cores or BIOS/shaders/overlays/thumbnails, launching RetroArch or any
content, executing the RetroArch binary or any core, RetroAchievements,
cloud sync, controller remapping content, save/state migration, and
frontend integration. This command only reports what already exists.
