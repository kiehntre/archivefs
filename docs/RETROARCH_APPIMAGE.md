# RetroArch AppImage Detection

Any distinct AppImage profile discovered here participates in the same bounded,
read-only existing-artifact inventory as native and Flatpak profiles; no
AppImage is executed, mounted, or extracted for inventory. See
[`RETROARCH_ARTIFACT_INVENTORY.md`](RETROARCH_ARTIFACT_INVENTORY.md).

`archivefs retroarch-environment` also detects RetroArch installed as an
AppImage - the primary way many users, including the one this feature was
built for, actually run RetroArch and other emulators on Linux. Detection
is strictly read-only: it never executes, mounts, extracts, or FUSE-mounts
an AppImage, never invokes an external tool (`unsquashfs`, `file`,
`readelf`, `strings`, `desktop-file-validate`, `appimagetool`, ...), never
scans the whole filesystem, and never writes, moves, renames, or modifies
an AppImage or a `.desktop` file. It builds directly on the same
`ReadOnlyHostFilesystem` trait, `EncodedPath`, and diagnostics model the
rest of `emulator_environment::retroarch` already uses - see
[`RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md) for that shared
foundation.

## Detection sources

Two independent, non-recursive, bounded directory listings feed detection:

**AppImage search roots** (`DiscoveryEnvironment::app_image_search_roots`),
five fixed, documented locations under `$HOME`, deliberately *not* `$HOME`
itself (scanning the whole home directory is out of scope):

```
~/Applications
~/.local/bin
~/.local/share/applications
~/AppImages
~/bin
```

Each is listed (non-recursively, final-component symlinks on individual
entries never followed) for files whose name ends in `.appimage`
case-insensitively, bounded at `MAX_APPIMAGE_SEARCH_ROOT_ENTRIES` (4096)
entries per directory. A missing search root is normal (most users won't
have all five) and produces only an `Info`-severity
`appimage_search_root_missing` diagnostic, never a warning - most of these
directories don't exist for a typical install. An unreadable root produces
`appimage_search_root_inaccessible`; a directory whose listing exceeds the
bound produces `appimage_directory_listing_too_large` and that root's
candidates are skipped rather than partially trusted.

**Desktop entry roots** (`DiscoveryEnvironment::desktop_file_roots`),
resolved per the XDG Base Directory Specification's own defaulting rules
(the same pattern this module already applies to `XDG_CONFIG_HOME`: unset,
empty, or relative values fall back to the documented default rather than
being resolved against an invented base):

```
$XDG_DATA_HOME/applications          (default: ~/.local/share/applications)
<dir>/applications for each dir in $XDG_DATA_DIRS
                                      (default: /usr/local/share:/usr/share)
```

Each is listed non-recursively for `*.desktop` files, bounded at
`MAX_DESKTOP_FILES_PER_DIRECTORY` (4096) per directory.

## AppImage candidate identification

A candidate is only ever reported when there is *some* real evidence it is
RetroArch - a bare `.appimage` file with an unrelated name and no matching
desktop entry produces no candidate at all (not even at `Unsupported`
confidence). Two evidence sources feed `AppImageIdentificationConfidence`:

- **Filename evidence**: the AppImage's own filename contains
  `retroarch` case-insensitively (e.g. `RetroArch.AppImage`,
  `retroarch-linux-x86_64.AppImage`). Filename alone is never promoted
  past `Weak`.
- **Desktop-entry evidence**: a parsed, *active* `.desktop` entry (see
  below) whose `Name`, `GenericName`, `Icon`, or `StartupWMClass` mentions
  `retroarch` case-insensitively.

```
pub enum AppImageIdentificationConfidence {
    Exact,        // desktop Name/Icon/... evidence AND Exec resolves to this exact file
    Strong,       // desktop Name/Icon/... evidence, but Exec is unresolved/wrapped/absent
    Weak,         // filename evidence only, no desktop evidence at all
    Ambiguous,    // 2+ distinct-config AppImages disagree - see "Profile deduplication"
    Unsupported,  // structural only; never produced for a reported candidate
}
```

**A desktop entry's `Exec` resolving to an AppImage's path is never, by
itself, treated as evidence that the AppImage is RetroArch.** This was a
real false-positive bug found during this feature's own real-world smoke
test: virtually every properly-installed AppImage ships its own ordinary
desktop entry whose `Exec` naturally points at itself (ES-DE, Heroic,
LosslessCut, Vita3K, and Eden were all initially, incorrectly flagged as
"Strong confidence RetroArch" purely because each had a normal,
self-referencing desktop file with nothing to do with RetroArch). The fix,
now the documented and tested rule: a desktop entry is only ever attached
as evidence for a candidate when *that entry itself* independently
mentions RetroArch by name/icon; a bare `Exec` match with no such mention
contributes nothing. See
`unrelated_appimages_own_self_pointing_desktop_entry_is_never_evidence` in
`emulator_environment::retroarch`'s test module for the regression test.

## Desktop entry parsing

`.desktop` files are parsed with the same conservative posture as
`retroarch.cfg`: bounded at `MAX_DESKTOP_FILE_BYTES` (256 KiB, producing
`desktop_file_too_large` when exceeded), required to be valid UTF-8
(`desktop_file_invalid_utf8` otherwise, file skipped), and a malformed
line is reported (`desktop_file_malformed_line`) without aborting the rest
of the parse. Only the `[Desktop Entry]` group's `Type`, `Name`,
`GenericName`, `Icon`, `StartupWMClass`, `Hidden`, `Exec`, and `TryExec`
keys are read. An entry with `Hidden=true`, or a `Type` other than
`Application`, is inactive (`desktop_entry_inactive`) and contributes no
evidence at all - `NoDisplay` is intentionally not treated as inactive,
since it only hides an entry from application menus, not from being a
genuine, currently-installed RetroArch launcher.

`Exec` is tokenized per the freedesktop.org Desktop Entry Specification's
quoting rules (unquoted whitespace-separated tokens; a double-quoted token
unescapes `"`, `` ` ``, `$`, `\`) and field codes (`%f`, `%F`, `%u`, `%U`,
`%i`, `%c`, `%k`, and the deprecated `%d`/`%D`/`%n`/`%N`/`%v`/`%m`) are
recognized and skipped when scanning for the executable token, never
treated as a literal path. A handful of known shell/env wrapper commands
(`sh`, `bash`, `env`, ...) are recognized: `env` is handled conservatively
by skipping leading `NAME=value` assignment tokens to find the real
target; `sh`/`bash` (and any other wrapper whose real target would require
executing a shell to discover) resolve to `ShellWrapperUnresolved` rather
than guessing. The resulting `ExecResolution` for a given candidate is one
of:

```
MatchesCandidate            // the Exec executable token is this exact candidate
MismatchedTarget { target } // Exec names a different, specific path
ShellWrapperUnresolved      // Exec is a shell wrapper; real target undeterminable
TargetMissing               // Exec names a specific path that does not exist
Unresolved                  // Exec absent, empty, or unparseable
```

## Executable state and symlink policy

`AppImageCandidate::executable` reports whether the *final path
component's* executable permission bit is set, checked with
`symlink_metadata` (never following the final component) via
`ReadOnlyHostFilesystem::probe_regular_file_executable_bit` - distinct
from the native-executable `PATH` lookup elsewhere in this module, which
does follow symlinks (a deliberate, existing, documented exception for
`PATH`-based discovery only). A candidate path whose final component is a
symlink is rejected outright (not reported as a candidate at all) and
produces an `appimage_candidate_symlink` diagnostic - AppImages are
ordinary regular files by convention, and a symlinked "AppImage" is
conservatively treated as unverified rather than followed.

## Configuration association

Every AppImage candidate carries a `ConfigAssociation` describing how its
RetroArch configuration relates to the native profile's own:

```
SharesNativeProfile               // no evidence of a distinct config root
PortableConfigDetected { path }   // <AppImage>.home or <AppImage>.config sibling dir exists
ExplicitConfig { path }           // a matching desktop Exec passes -c/--config <resolved-path>
Unknown                           // an explicit --config value exists but could not be resolved
Ambiguous                         // 2+ AppImages disagree on distinct config directories
```

- **Portable-mode sibling directories** (verified against the official
  AppImage runtime source, `AppImage/type2-runtime`'s `runtime.c`,
  `set_portable_home_and_config`): if `<AppImage-path>.home` and/or
  `<AppImage-path>.config` exists as a directory next to the AppImage, the
  AppImage runtime itself overrides `$HOME`/`$XDG_CONFIG_HOME` for the
  launched process - either sibling alone is sufficient evidence of a
  distinct config root.
- **Explicit `--config`/`-c`** (verified against `retroarch.c`'s own
  argument parser): scanned from a matching desktop entry's tokenized
  `Exec` value. Takes precedence over the portable-mode convention when
  both are present, since RetroArch's own `-c` handling would override
  whatever the AppImage runtime set up. A value that can't be resolved to
  a real path (e.g. a bare field code) is `Unknown`, never guessed.

## Profile deduplication

This milestone deliberately supports **at most one** additional, distinct
AppImage profile - not an open-ended multi-profile identity model. The
common case (an AppImage shares the native profile's configuration, which
is the default when no portable-mode or explicit-config evidence exists)
is purely additive: the candidate is attached to the *existing* native
profile's `app_images` field, with **zero new profiles created and zero
new profile-array elements**. A genuinely new `ProfileKind::AppImage`
profile is only created when:

1. At least one candidate has verified evidence of a distinct config
   directory (`PortableConfigDetected` or `ExplicitConfig`), and
2. Every candidate with such evidence agrees on the *same* distinct
   directory (by real path bytes, never by display-string comparison, so
   two lossy-equal non-UTF-8 paths are correctly treated as distinct).

If two or more AppImages have distinct-config evidence but **disagree**
on the directory, no new profile is created at all - every affected
candidate is folded back onto the native profile with
`ConfigAssociation::Ambiguous`, and a `duplicate_logical_profile_prevented`
diagnostic is emitted. This is a deliberate "do not guess" scope
reduction: resolving which of several disagreeing AppImages is the "real"
one would require executing them, which this milestone never does.

Two AppImages that both point at the *same* real candidate path (e.g. via
two overlapping search roots) are deduplicated to one entry using the
real path's bytes, never a lossy display string. Flatpak profiles never
receive AppImage evidence under any circumstance - the two installation
mechanisms are always kept separate.

## Environment report integration

`RetroArchProfile` gained one new field, `app_images: Vec<AppImageCandidate>`
- always empty for both Flatpak profiles. `RetroArchEnvironmentReport.profiles`
now contains **3 or 4** entries:

- No distinct AppImage profile: still exactly 3, in the existing
  native/Flatpak-user/Flatpak-system order, with any shared-config
  AppImage candidates attached to the native profile's own `app_images`.
- A distinct AppImage profile exists: 4 entries, with the new
  `ProfileKind::AppImage` profile inserted **between** native and
  Flatpak-user (native, AppImage, Flatpak-user, Flatpak-system) - `ProfileKind`'s
  own `Ord` derivation places `AppImage` between `Native` and `Flatpak`,
  and profile sorting already relies on that derived order.

Because this changes a documented "always exactly 3, at these fixed
positions" *positional* array contract - not just adding a new object
field - `RetroArchEnvironmentReport::format_version` is bumped from `1` to
`2`. Per this project's JSON compatibility policy
([`json-api.md`](json-api.md)), purely additive object fields never
require a version bump, but this is different: an existing consumer that
assumed `profiles[2]` is always Flatpak-system would silently read the
wrong profile once a 4th, AppImage entry is inserted at index 1. See
[`RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md)'s JSON contract
section for the full profile ordering rules.

`retroarch-patch-preview`'s `RetroArchAdvisoryPlan.profile_outcomes[]`
follows suit automatically: it is produced by iterating
`environment.profiles[]` generically (no hardcoded profile count or kind
anywhere in `patch_manager::retroarch`'s core orchestration), so it is
also 3 or 4 entries, in the same order, with **zero code changes** needed
in that module beyond one `profile_kind_tag` match arm for the plan-ID
hash (`ProfileKind::AppImage => b"app_image"`). `RetroArchAdvisoryPlan`'s
own top-level `format_version` stays `1` - only the *embedded*
`environment.format_version` changed, and that change is itself hashed
into `plan_id` like every other environment field, so an AppImage-bearing
environment naturally produces a different plan ID than a non-AppImage
one for the same catalogue, without any separate plan-level version bump.

## CLI output

`archivefs retroarch-environment`'s human output gains an "AppImage
candidates:" section per profile (only printed when non-empty), compact
by design to match the rest of this command's style: each candidate's
path, confidence, executable state, and a short configuration-association
summary, followed by a nested, equally compact line per desktop-entry
match (desktop file path, name-evidence flag, exec resolution) - never a
full dump of every parsed desktop-entry field. No new top-level CLI
command or flag was added; only `--json` is accepted, unchanged.

## Determinism

AppImage candidates are sorted by encoded path bytes, matching every other
list in this module - never by filesystem enumeration order, and never by
a lossy display string. `report_is_deterministic_across_repeated_calls`
and `appimage_candidates_are_sorted_deterministically` cover this.

## Read-only guarantees

No AppImage is executed, mounted, extracted, or FUSE-mounted. No
`.desktop` file, AppImage, or any other file is created, written, renamed,
or deleted. No external tool is invoked. No process is spawned. No
network call is made. All AppImage/desktop-file reads are bounded and
existence-probing only, through the same `ReadOnlyHostFilesystem` trait as
the rest of this module. `no_appimage_or_desktop_file_is_ever_modified`
and `discovery_makes_no_filesystem_writes` compare the exact filesystem
tree before and after a discovery run.

## Non-goals

- Reading an AppImage's embedded SquashFS payload, `.desktop` file, or
  icon (would require mounting or extracting the AppImage - out of
  scope).
- Verifying an AppImage's digital signature or update information.
- Discovering AppImages via `appimaged`/`AppImageLauncher`'s own
  integration database, or via desktop-menu indexing services - only the
  fixed default search roots and XDG desktop-entry roots documented above
  are scanned.
- More than one distinct AppImage profile - see "Profile deduplication."
- Any AppImage discovery on Windows or macOS - Linux-only, matching this
  module's existing scope.
- Modifying, repairing, creating, or "adopting" a `.desktop` file to make
  an AppImage more discoverable later.

## Packaging-dependent unknowns

- Whether a given AppImage is a type 1 or type 2 AppImage is not
  determined (would require reading the ELF header/embedded filesystem
  signature) and does not affect detection, since portable-mode's
  `.home`/`.config` sibling-directory convention is a type2-runtime
  feature specifically; a type 1 AppImage's own portable-mode support (if
  any) has not been verified against a primary source and is not assumed.
- Whether `appimaged`/`AppImageLauncher` (if installed) has already
  registered a desktop entry for a given AppImage elsewhere on the system
  is unknown; only the fixed roots above are scanned, so an AppImage
  integrated solely through a mechanism outside those roots will not gain
  desktop-entry evidence (filename evidence, if present, still applies).
