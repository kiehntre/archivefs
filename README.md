<img width="1024" height="559" alt="archivefs-banner" src="https://github.com/user-attachments/assets/aa1816c0-316c-4c1b-986e-ba7c14bae9c5" />

# ArchiveFS

ArchiveFS is a Linux-first, local-first tool for browsing, mounting,
inspecting, validating, and organizing archived collections you already
have - games, software, media, documents, or other preservation material -
without extracting everything permanently. It is alpha-stage software under
active development.

**ArchiveFS is not:** a game or ROM download service, a storefront, a
source of BIOS/firmware/copyrighted content, a cloud account service, a DRM
platform, an emulator replacement, or a universal launcher/frontend
replacement. See [Current limitations](#current-limitations) and
[`ROADMAP.md`](ROADMAP.md#explicitly-out-of-scope-for-now) for the full,
explicit list of what it deliberately does not do.

**Release status:** `v0.5.0-alpha` is currently in preparation - a hardening
release (mount lifecycle postcondition checks, transactional catalogue
refresh, RetroArch cheat-source cache locking) plus a redesigned desktop
GUI and a first-class Cheats & Mods workspace that now spans **three
read-only emulator adapters**: RetroArch and PCSX2, plus Dolphin, which
has been implemented and validated but is **not yet merged into this
branch** (see the release notes' "Dolphin read-only adapter" section
before assuming it is present in any build from this branch today).
Further emulator adapter expansion is paused after Dolphin for now - see
[`ROADMAP.md`](ROADMAP.md#medium-term-plans). See
[`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md)
for what's actually new and what remains unavailable, and
[`docs/MANUAL_QA_v0.5.0-alpha.md`](docs/MANUAL_QA_v0.5.0-alpha.md) for the
manual acceptance checklist. Nothing here has been tagged yet - the
workspace version in `Cargo.toml` still reads `0.4.3-alpha` until that
release-checklist step happens.

## Principles

- **Local-first.** No telemetry, no required cloud account, and it keeps
  working offline.
- **Read-only by default.** Archives are mounted read-only; ArchiveFS never
  modifies your source archive files.
- **Explicit over automatic.** Mounting, unmounting, cleanup, patch preview,
  and library-view changes are all explicit user actions - nothing silently
  mounts, unmounts, downloads, or rewrites emulator configuration on your
  behalf.
- **Transparent.** No secret scanning, no remote kill switches, no hidden
  writes, and no service deciding on your behalf which files are
  "acceptable." You remain responsible for your own files.

See [`docs/security.md`](docs/security.md) and [`SECURITY.md`](SECURITY.md)
for the detailed safety model behind these principles.
Cheat and mod trust, local inspection, unknown-code, privacy, original-file,
and responsible-use boundaries are documented separately in
[`docs/CHEATS_MODS_SAFETY.md`](docs/CHEATS_MODS_SAFETY.md), with a shorter
user-facing version at
[`docs/CHEATS_MODS_USER_POLICY.md`](docs/CHEATS_MODS_USER_POLICY.md).

## What ArchiveFS does today

- Safely scans absolute, non-symlinked configured source folders for supported archives: `.zip`, `.7z`, and `.rar` (skipping symlink/special-file entries and obvious split-archive continuation parts, with bounded traversal).
- Mounts archives read-only through `ratarmount`, individually or in bulk, with safe mount-name generation, lazy-unmount recovery, and cleanup of empty mount directories.
- Maintains a persistent, local SQLite catalogue of your library (`library-scan`, `library-list`, `library-find`, `library-status`, `health`) so commands don't need to rescan the filesystem every time - this catalogue is additive and is never consulted for mount/unmount safety decisions. Catalogue reports and previews use an explicit read-only open; `database-check` additionally distinguishes hot-header evidence, zeroed/truncated non-hot journals, malformed headers, and recovery-required read-only failures without creating, migrating, repairing, or checkpointing anything.
- Supports multiple independent source folders (`sources`, `source add/enable/disable/scan/remove`).
- Detects platform from filenames and folder-name aliases, with manual overrides (`library-set-platform`) and persistent custom aliases (`platform-alias-*`) that outrank automatic detection.
- Reports filename-based duplicate candidates (`duplicates`) - a read-only report, never an automatic cleanup.
- Builds **managed Library Views**: named, symlink-based organized views of your catalogue (for example, grouped by platform) in a separate directory tree, without moving, copying, or extracting your archives. See [`docs/library-views.md`](docs/library-views.md).
- Provides a **read-only PCSX2 patch-preview** (`pcsx2-patch-preview`): fetches official PCSX2 patch metadata and shows native/Flatpak installation *candidates* as a non-executable advisory plan. It does not download, verify, install, or enable any patch. PCSX2 is the only implemented `EmulatorAdapter` trait implementation - see [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md).
- Provides **read-only RetroArch environment discovery** (`retroarch-environment`): detects native and Flatpak RetroArch profiles, parses `retroarch.cfg` for a fixed set of configured paths, and inventories installed cores. It makes no filesystem changes, spawns no process, and makes no network call - see [`docs/RETROARCH_ENVIRONMENT.md`](docs/RETROARCH_ENVIRONMENT.md).
- Provides a **read-only RetroArch cheat/patch destination preview and existing-artifact inventory** (`retroarch-patch-preview`): for every catalogued game, previews where a per-game `.cht` cheat file or IPS/BPS/UPS/Xdelta soft-patch sibling file would go, then safely inventories supported files already present, including occupied, duplicate, conflicting, ambiguous, and orphaned states. Builds on the environment discovery above; makes no network call at all and does not implement `EmulatorAdapter` (RetroArch's shape doesn't fit that PCSX2-specific trait) - see [`docs/RETROARCH_PATCH_PREVIEW.md`](docs/RETROARCH_PATCH_PREVIEW.md) and [`docs/RETROARCH_ARTIFACT_INVENTORY.md`](docs/RETROARCH_ARTIFACT_INVENTORY.md).
- Strengthens that preview with **read-only RetroArch playlist matching**: parses your existing `.lpl` playlists (never writing or modifying them) to link content and cores with real evidence instead of file-extension guessing alone, resolving ambiguous core matches when the evidence is unambiguous - see [`docs/RETROARCH_PLAYLISTS.md`](docs/RETROARCH_PLAYLISTS.md).
- Detects **RetroArch installed as an AppImage** (`retroarch-environment`): scans a fixed set of default locations and your XDG desktop-entry directories, read-only and non-recursive, and feeds any found AppImage into the same environment/playlist/patch-preview pipeline as a native install - without ever executing, mounting, or extracting the AppImage, and without creating a duplicate profile when it shares your existing RetroArch configuration. See [`docs/RETROARCH_APPIMAGE.md`](docs/RETROARCH_APPIMAGE.md).
- Provides safe RetroArch cheat installation and journal-driven rollback, plus
  read-only installation history and single-journal assessment through
  `retroarch-cheat-history` and `retroarch-cheat-inspect`. Inspection validates
  current destination and backup hashes without changing files; see
  [`docs/RETROARCH_CHEAT_HISTORY.md`](docs/RETROARCH_CHEAT_HISTORY.md).
- Provides guided local or trusted-source RetroArch cheat setup through
  `retroarch-cheat-setup <catalogue-path>` or `retroarch-cheat-setup --source
  <source-id>`: discovers safe native, Flatpak, and
  verified portable profiles, previews conservative matches, and delegates
  approved changes to the existing journaled installer. See
  [`docs/RETROARCH_CHEAT_SETUP.md`](docs/RETROARCH_CHEAT_SETUP.md).
- Retrieves reviewed remote catalogues separately with
  `retroarch-cheat-source-list`, `retroarch-cheat-source-fetch`, and
  `retroarch-cheat-source-inspect`. Fetching produces a bounded, validated,
  immutable local snapshot and never installs cheats. See
  [`docs/RETROARCH_CHEAT_SOURCES.md`](docs/RETROARCH_CHEAT_SOURCES.md).
- Presents Cheats & Mods as a first-class GUI workspace while keeping profile,
  source trust, inspection, destination, and installation state distinct. Its
  in-page picker changes only workspace context; it can inventory an eligible
  profile's existing cheat directory with fixed read-only bounds or retrieve a
  trusted cached catalogue. For PS2 archives it also offers a read-only PCSX2
  adapter that discovers safe native/Flatpak profiles and inventories existing
  `cheats`, `cheats_ws`, and present `patches` PNACH files. A shared bounded
  ISO reader can derive a verified PS2 serial and, when the complete boot ELF
  fits its limit, PCSX2's executable CRC. GameCube and Wii
  archives can use a similarly read-only Dolphin adapter to discover native or
  Flatpak user directories and inspect bounded `GameSettings/*.ini` metadata.
  Verified Dolphin Game IDs can establish exact INI matches. Neither adapter
  inspects arbitrary local imports or installs content; see
  [`docs/CHEATS_MODS_SAFETY.md`](docs/CHEATS_MODS_SAFETY.md),
  [`docs/PCSX2_READONLY_ADAPTER.md`](docs/PCSX2_READONLY_ADAPTER.md),
  [`docs/DOLPHIN_READONLY_ADAPTER.md`](docs/DOLPHIN_READONLY_ADAPTER.md), and
  [`docs/SHARED_GAME_IDENTITY.md`](docs/SHARED_GAME_IDENTITY.md). A shared,
  bounded source-to-destination preview reports missing, identical, different,
  unsafe, ambiguous, and conflicting states without changing files; see
  [`docs/SHARED_CHEAT_PREVIEW.md`](docs/SHARED_CHEAT_PREVIEW.md).
- Inventories, verifies, pins and deliberately prunes immutable cheat-source
  snapshots with preview-first cache maintenance. Current, last-known-good and
  pinned snapshots remain protected, and retrieval and maintenance coordinate
  through one bounded cross-process cache lock; see
  [`docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md`](docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md).
- Builds a JSON index and watches source folders to keep it fresh, without ever auto-mounting or auto-unmounting.
- Includes config validation and doctor-style diagnostics.
- Ships a desktop GUI (`archivefs-gui`) covering scanning, mounting, sources, library views, duplicates, and catalogue health over the same core logic as the CLI.
- Provides stable, documented JSON output for several commands - see [`docs/json-api.md`](docs/json-api.md).

## Current limitations

- No automatic patch installation or cheat enabling -
  `pcsx2-patch-preview` and `retroarch-patch-preview` are preview only.
  Guided cheat setup installs only after confirmation and never enables cheats;
  trusted retrieval remains separate from installation.
- No broad multi-emulator support yet - PCSX2, RetroArch, and Dolphin are the
  only emulators with any patch/cheat preview or inventory today, and none is
  launched or configured by these read-only workflows.
- Not every archive format, Linux distribution, emulator, or frontend is
  supported or tested - see [Supported/tested environments](#supportedtested-environments-and-formats).
- No automatic modification of emulator configuration files.
- No official distribution of games, ROMs, BIOS, firmware, or patches -
  ArchiveFS organizes and previews collections you already have.
- This is alpha software: workflows may be incomplete, and defects should
  be expected. See [`CHANGELOG.md`](CHANGELOG.md) for what has actually
  shipped.

## Install from a Release

Prebuilt Linux binaries are published on the [Releases](https://github.com/kiehntre/archivefs/releases) page for tagged versions, for example `v0.4.3-alpha`. This is the quickest way to get running without building from source.

1. Download the release tarball and its `SHA256SUMS` file, for example:

   ```sh
   curl -LO https://github.com/kiehntre/archivefs/releases/download/v0.4.3-alpha/archivefs-v0.4.3-alpha-x86_64-linux.tar.gz
   curl -LO https://github.com/kiehntre/archivefs/releases/download/v0.4.3-alpha/SHA256SUMS
   ```

2. Verify the tarball against the checksum file before extracting it:

   ```sh
   sha256sum -c SHA256SUMS --ignore-missing
   ```

3. Extract it:

   ```sh
   tar -xzf archivefs-v0.4.3-alpha-x86_64-linux.tar.gz
   cd archivefs-v0.4.3-alpha-x86_64-linux
   ```

### Quick install

From inside the extracted directory, run the installer:

```sh
./install.sh
```

This installs `archivefs-cli` and `archivefs-gui` into `~/.local/bin` (override the location with `--prefix PATH`), creates `~/.config/archivefs`, and copies `config.toml.example` to `config.toml` there - but only if a config does not already exist; an existing config is never touched. It uses no `sudo` and does not modify your shell startup files. It also checks whether `ratarmount` is on `PATH` and prints installation guidance if it is not. It is safe to run again later (for example after upgrading to a newer release tarball).

Edit `source_folders` and `mount_root` in `~/.config/archivefs/config.toml`, then run `archivefs-cli doctor` (see the PATH note below if that command is not found).

To remove what it installed (your config is left in place):

```sh
./install.sh --uninstall
```

Pass the same `--prefix PATH` to `--uninstall` if you installed to a non-default location. Run `./install.sh --help` for the full list of options.

**PATH note:** the installer never edits shell startup files, so if `~/.local/bin` is not already on your `PATH`, add it yourself - for example add this line to `~/.bashrc` or `~/.zshrc`, then restart your shell (or `source` that file):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Until then, run the installed binaries with their full path: `~/.local/bin/archivefs-cli doctor`.

### Manual installation

Manual installation remains available if you would rather control each step yourself, or need to install somewhere the script does not handle:

4. Make the binaries executable, if extraction did not already preserve that:

   ```sh
   chmod +x archivefs-cli archivefs-gui
   ```

5. Install `ratarmount` separately. It is an external dependency that ArchiveFS shells out to for mounting - it is not bundled in the release tarball, and archive mounting will not work without it. Install it however fits your system, then make sure the `ratarmount` command is on your `PATH` (or point `ratarmount_bin` in the config at its full path).

6. Copy the example configuration and edit it for your system:

   ```sh
   mkdir -p ~/.config/archivefs
   cp config.toml.example ~/.config/archivefs/config.toml
   ```

   Edit `source_folders` and `mount_root` in `~/.config/archivefs/config.toml` to point at real paths on your machine.

7. Check that everything is set up correctly:

   ```sh
   ./archivefs-cli doctor
   ./archivefs-cli config-check
   ```

8. Launch the desktop GUI, if you want it:

   ```sh
   ./archivefs-gui
   ```

   `archivefs-gui` needs a running Linux desktop session (X11 or Wayland) with the usual runtime graphics libraries present - it will not open a window over a bare SSH session or on a headless server with no desktop environment.

Archive mounts created by ArchiveFS are always read-only; it never modifies files in your configured `source_folders`.

There is currently no package-manager distribution of ArchiveFS (no apt, dnf, pacman, Homebrew, or similar package) - the release tarball above and building from source below are the two supported ways to install it.

## Supported/tested environments and formats

- **Platform:** Linux only. Mount and watcher behavior rely on Linux
  facilities (`/proc/self/mountinfo`, FUSE-style mount tools, `inotify` via
  the `notify` crate). macOS and Windows are not supported.
- **Archive formats:** `.zip`, `.7z`, and `.rar` (with split-RAR
  continuation-part skipping). No other archive formats are currently
  detected or mounted.
- **Mount backend:** `ratarmount` only, invoked as an external tool - not
  bundled, must be installed and on `PATH` separately.
- **Desktop GUI:** requires a running X11 or Wayland session; there is no
  headless mode.
- This list reflects what the code and tests currently exercise, not an
  exhaustive compatibility guarantee across every Linux distribution.

## Build from Source (Developers)

ArchiveFS is a Rust workspace. It pins an exact Rust toolchain via
[`rust-toolchain.toml`](rust-toolchain.toml) - if you have `rustup`
installed, it will install and use that exact version automatically inside
this repository. See [`CONTRIBUTING.md`](CONTRIBUTING.md#rust-toolchain-policy)
for the full toolchain policy.

Build the CLI from source:

```sh
cargo build --workspace
```

The development binary will be at:

```sh
target/debug/archivefs-cli
```

For regular local use, install it with Cargo:

```sh
cargo install --path crates/archivefs-cli
```

Run the full validation suite before submitting changes:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace --release --locked
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for more on making changes.

## Desktop GUI

ArchiveFS also includes a desktop frontend built with `egui`/`eframe`. It scans in the background and shows archive totals, mount states, doctor checks, paths, platforms, sources, catalogue duplicates and health, and searchable status rows, with the same read-only-by-default safety model as the CLI.

Build and run it from the workspace root:

```sh
cargo build -p archivefs-gui
cargo run -p archivefs-gui
```

The GUI uses the same `~/.config/archivefs/config.toml` configuration and core scanning/catalogue logic as the CLI. Use **Refresh** to rescan after filesystem or mount-state changes.

Archive mounting uses `ratarmount`, so install it separately and make sure it is available on `PATH`, or set `ratarmount_bin` in the config.

## Configuration

ArchiveFS reads its default config from:

```text
~/.config/archivefs/config.toml
```

Example:

```toml
source_folders = ["/data/archives"]
mount_root = "/mnt/archivefs"
ratarmount_bin = "ratarmount"
```

The same example, with comments, ships as [`config.toml.example`](config.toml.example) in this repository and in every release tarball - copy it to `~/.config/archivefs/config.toml` as a starting point.

`source_folders` are scanned recursively. `mount_root` is where ArchiveFS creates planned mount directories. ArchiveFS does not modify files in `source_folders`. `ratarmount_bin` is optional and defaults to `"ratarmount"` resolved from `PATH`.

**Note on syntax:** ArchiveFS uses a small hand-written config parser, not a full TOML implementation. `source_folders = ["/data/archives"]` on one line (shown above) always works. Splitting the array across multiple lines, e.g.:

```toml
source_folders = [
  "/data/archives",
  "/data/more-archives",
]
```

is also accepted, but only this `key = "value"` / `key = [...]` form is understood - there is no support for TOML tables, inline tables, or nested arrays.

Managed Library Views and persistent multi-source configuration use their
own JSON files under `~/.config/archivefs/` (`library_views.json`,
`sources.json`) - see [`docs/library-views.md`](docs/library-views.md).

## Common Commands

Scanning, status, and mounting:

```sh
archivefs-cli doctor
archivefs-cli config-check
archivefs-cli scan
archivefs-cli status
archivefs-cli stats
archivefs-cli info "007 Legends"
archivefs-cli mount-one "007 Legends"
archivefs-cli unmount-one "007 Legends"
archivefs-cli duplicates
archivefs-cli index-build
archivefs-cli index-show
archivefs-cli index-find "xbox360"
archivefs-cli watch
```

Persistent catalogue and multi-source management:

```sh
archivefs-cli library-status
archivefs-cli database-check
archivefs-cli database-check --json
archivefs-cli library-scan
archivefs-cli library-list
archivefs-cli library-find "007 Legends"
archivefs-cli library-set-platform "Luigi's Mansion" GameCube
archivefs-cli platform-alias-add gc GameCube
archivefs-cli sources
archivefs-cli source add /data/more-archives
archivefs-cli source scan-all
```

Managed library views:

```sh
archivefs-cli view list
archivefs-cli view preview "By Platform"
archivefs-cli view apply "By Platform"
```

Patch preview:

```sh
archivefs-cli pcsx2-patch-preview
archivefs-cli pcsx2-patch-preview --json
```

RetroArch environment discovery:

```sh
archivefs-cli retroarch-environment
archivefs-cli retroarch-environment --json
```

RetroArch cheat/patch destination preview:

```sh
archivefs-cli retroarch-patch-preview
archivefs-cli retroarch-patch-preview --json
```

RetroArch cheat installation history:

```sh
archivefs-cli retroarch-cheat-history
archivefs-cli retroarch-cheat-history --json
archivefs-cli retroarch-cheat-inspect ~/.local/share/archivefs/cheat-install-runs/<run>.json
```

Use verbose or debug logging when you need more detail:

```sh
archivefs-cli --verbose stats
archivefs-cli --debug watch
```

Run `archivefs-cli --help` for the complete, current command list with
descriptions.

## Typical Workflow

1. Create `~/.config/archivefs/config.toml`.
2. Run `archivefs-cli config-check` to validate the config.
3. Run `archivefs-cli doctor` to check source folders, mount root, tools, and current archive state.
4. Run `archivefs-cli library-scan` to build the persistent catalogue, then `archivefs-cli stats` or `archivefs-cli library-list` to inspect what ArchiveFS sees.
5. Run `archivefs-cli info "name"` to inspect one archive.
6. Run `archivefs-cli mount-one "name"` to mount a single archive.
7. Run `archivefs-cli unmount-one "name"` when finished.
8. Optionally set up a Library View (`archivefs-cli view preview`/`apply`) for an organized, browsable directory tree.
9. Run `archivefs-cli watch` if you want ArchiveFS to refresh the JSON index when source folders change.

## Example Output

`archivefs-cli stats`:

```text
ArchiveFS Stats

Summary:
  Total archives: 128
  Mounted: 3
  Pending: 125
  Total archive size: 42.8 GiB

Platforms:
  Unknown: 12
  Xbox360: 116

Archive extensions:
  7z: 44
  rar: 9
  zip: 75
```

`archivefs-cli info "007 Legends"`:

```text
ArchiveFS Info

Details:
  Title: 007 Legends
  Platform: Xbox360
  Archive path: /data/archives/xbox360/007 Legends.zip
  Mount path: /mnt/archivefs/Xbox360/007_Legends
  Extension: zip
  Archive size: 7.4 GiB
  Last modified: 2026-06-01 14:22:10 UTC
  Health: Pending
  Mount state: Pending
  Metadata provider: FilenameMetadataProvider
  Health provider: FilesystemHealthProvider
```

`archivefs-cli index-show`:

```text
ArchiveFS Index

Summary:
  Total archives: 128
  Mounted: 3
  Pending: 125

Platforms:
  Unknown: 12
  Xbox360: 116
```

## Documentation

- [Architecture overview](ARCHITECTURE.md) / [full architecture reference](docs/architecture.md)
- [Roadmap](ROADMAP.md)
- [Changelog](CHANGELOG.md)
- [Domain model](docs/domain-model.md)
- [Persistent database](docs/database.md) / [database design](docs/DATABASE_DESIGN.md) / [ADR 0001](docs/adr/0001-persistent-library-database.md)
- [Managed library views](docs/library-views.md)
- [Patch & cheat manager design (PCSX2 preview, adapter boundary)](docs/PATCH_CHEAT_MANAGER_DESIGN.md)
- [Read-only PCSX2 Cheats & Mods adapter](docs/PCSX2_READONLY_ADAPTER.md)
- [Read-only Dolphin Cheats & Mods adapter](docs/DOLPHIN_READONLY_ADAPTER.md)
- [Shared verified game identity](docs/SHARED_GAME_IDENTITY.md)
- [Shared read-only Cheats & Mods preview](docs/SHARED_CHEAT_PREVIEW.md)
- [RetroArch environment discovery](docs/RETROARCH_ENVIRONMENT.md)
- [RetroArch cheat/patch destination preview](docs/RETROARCH_PATCH_PREVIEW.md)
- [RetroArch existing cheat/patch artifact inventory](docs/RETROARCH_ARTIFACT_INVENTORY.md)
- [RetroArch playlist identity and content matching](docs/RETROARCH_PLAYLISTS.md)
- [RetroArch AppImage detection](docs/RETROARCH_APPIMAGE.md)
- [RetroArch cheat installation history and journal inspection](docs/RETROARCH_CHEAT_HISTORY.md)
- [RetroArch guided cheat setup](docs/RETROARCH_CHEAT_SETUP.md)
- [RetroArch cheat installer and install-result model](docs/RETROARCH_CHEAT_INSTALL.md) / [install result](docs/RETROARCH_CHEAT_INSTALL_RESULT.md)
- [RetroArch cheat rollback](docs/RETROARCH_CHEAT_ROLLBACK.md)
- [Trusted RetroArch cheat-source retrieval](docs/RETROARCH_CHEAT_SOURCES.md) / [cheat catalogue](docs/RETROARCH_CHEAT_CATALOGUE.md)
- [RetroArch cheat-source cache maintenance](docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md) / [cache locking](docs/RETROARCH_CHEAT_CACHE_LOCKING.md)
- [Cheats & Mods trust, safety, and privacy model](docs/CHEATS_MODS_SAFETY.md) / [user-facing policy](docs/CHEATS_MODS_USER_POLICY.md)
- [Watcher](docs/watcher.md)
- [Provider pipeline](docs/provider-pipeline.md)
- [Duplicate detector](docs/duplicate-detector.md)
- [Security model](docs/security.md)
- [JSON API](docs/json-api.md)
- [v0.5.0-alpha release notes](docs/RELEASE_NOTES_v0.5.0-alpha.md)
- [v0.5.0-alpha manual QA plan](docs/MANUAL_QA_v0.5.0-alpha.md)
- [Release checklist](docs/release-checklist.md)
- [Paper cuts / small usability notes](docs/paper-cuts.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy / reporting](SECURITY.md)
- [Vision](VISION.md)

## Dedication

ArchiveFS is dedicated to [my dad](DEDICATION.md).
