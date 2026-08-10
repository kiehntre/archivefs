<img width="1024" height="559" alt="emuwiz-banner" src="https://github.com/user-attachments/assets/aa1816c0-316c-4c1b-986e-ba7c14bae9c5" />

# EmuWiz

EmuWiz is a Linux-first, local-first tool for browsing, mounting,
inspecting, validating, and organizing archived collections you already
have - games, software, media, documents, or other preservation material -
without extracting everything permanently. It is alpha-stage software under
active development.

EmuWiz was previously known as ArchiveFS.

**EmuWiz is not:** a game or ROM download service, a storefront, a
source of BIOS/firmware/copyrighted content, a cloud account service, a DRM
platform, an emulator replacement, or a universal launcher/frontend
replacement. See [Current limitations](#current-limitations) and
[`ROADMAP.md`](ROADMAP.md#explicitly-out-of-scope-for-now) for the full,
explicit list of what it deliberately does not do.

**Release status:** the current release is `v0.7.0`. It adds
the canonical 74-platform registry, evidence-based confidence, bounded Atari
ST/Wii/PS2/GameCube identity, safer Dolphin profile resolution, PS2/GameCube/Wii
GameHacking workflows, expanded Doctor diagnostics, and the optional browse-only
BSFree Archive source. See
[`docs/releases/v0.7.0.md`](docs/releases/v0.7.0.md) for the full release notes.

## Principles

- **Local-first.** No telemetry, no required cloud account, and it keeps
  working offline.
- **Read-only by default.** Archives are mounted read-only; EmuWiz never
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

## What EmuWiz does today

- Safely scans absolute, non-symlinked configured source folders for supported
  archives (`.zip`, `.7z`, `.rar`) and supported direct game images, including
  GameCube/Wii `.iso`, `.gcm`, `.gcz`, `.rvz`, `.wbfs`, and `.ciso`. Scanning
  is bounded and read-only; images are never mounted, converted, or modified.
- Mounts archives read-only through `ratarmount`, individually or in bulk, with safe mount-name generation, lazy-unmount recovery, and cleanup of empty mount directories.
- Maintains a persistent, local SQLite catalogue of your library (`library-scan`, `library-list`, `library-find`, `library-status`, `health`) so commands don't need to rescan the filesystem every time - this catalogue is additive and is never consulted for mount/unmount safety decisions. Catalogue reports and previews use an explicit read-only open; `database-check` additionally distinguishes hot-header evidence, zeroed/truncated non-hot journals, malformed headers, and recovery-required read-only failures without creating, migrating, repairing, or checkpointing anything.
- Supports multiple independent source folders (`sources`, `source add/enable/disable/scan/remove`).
- Detects platforms through one canonical 74-platform/311-alias registry and
  reports `Confirmed`, `Probable`, `Ambiguous`, or `Unknown` evidence. Manual
  assignments (`library-set-platform`) and persistent custom aliases
  (`platform-alias-*`) outrank automatic detection. Structurally valid evidence
  is separately marked conclusive or non-conclusive so, for example, a valid
  FAT12 `.st` image is not treated as uniquely Atari ST without corroboration.
- Validates Atari ST `.st` geometry and `.stx`/Pasti containers through bounded,
  cancellable, read-only inspection. Generic `.dsk`, `.bin`, and `.iso` remain
  ambiguous unless their own platform evidence settles them.
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
- Audits existing Dolphin and RetroArch provider coverage with
  `cheat-provider-coverage`: an exact-ID, at-most-32-game, read-only report
  showing compatible/rejected counts, duplicates, conflicts, unsupported
  formats, and honest no-match reasons without exposing local paths. See
  [`docs/CHEAT_PROVIDER_COVERAGE.md`](docs/CHEAT_PROVIDER_COVERAGE.md).
- Presents Cheats & Mods as a first-class GUI workspace while keeping profile,
  source trust, inspection, destination, and installation state distinct. Its
  in-page picker changes only workspace context; it can inventory an eligible
  profile's existing cheat directory with fixed read-only bounds or retrieve a
  trusted cached catalogue. For PS2 archives it also offers a read-only PCSX2
  adapter that discovers safe native/Flatpak profiles and inventories existing
  `cheats`, `cheats_ws`, and present `patches` PNACH files. A shared bounded
  ISO reader can derive a verified PS2 serial and, when the complete boot ELF
  fits its limit, PCSX2's executable CRC. GameCube and Wii
  archives can use the Dolphin adapter to discover native or Flatpak user
  directories, inspect bounded `GameSettings/*.ini` metadata, and retrieve
  exact-ID Gecko definitions from Dolphin's official upstream dataset.
  Verified Dolphin Game IDs and revisions bind the provider lookup and exact
  destination. PCSX2 does not install content; Dolphin can install selected
  validated Gecko definitions after preview and confirmation. Neither adapter
  inspects arbitrary local imports; see
  [`docs/CHEATS_MODS_SAFETY.md`](docs/CHEATS_MODS_SAFETY.md),
  [`docs/PCSX2_READONLY_ADAPTER.md`](docs/PCSX2_READONLY_ADAPTER.md),
  [`docs/DOLPHIN_READONLY_ADAPTER.md`](docs/DOLPHIN_READONLY_ADAPTER.md), and
  [`docs/SHARED_GAME_IDENTITY.md`](docs/SHARED_GAME_IDENTITY.md). A shared,
  bounded source-to-destination preview reports missing, identical, different,
  unsafe, ambiguous, and conflicting states without changing files; see
  [`docs/SHARED_CHEAT_PREVIEW.md`](docs/SHARED_CHEAT_PREVIEW.md).
- For RetroArch specifically, an eligible exact or approved-strong trusted-
  catalogue match can go further: the GUI can apply it through a shared,
  locked transaction engine after explicit confirmation (with a separate,
  non-preselected approval before replacing different existing content),
  verify the write, and record a journal entry. History & Logs can open that
  exact operation and preview/confirm its rollback. EmuWiz never
  auto-applies. PCSX2 remains preview-only. Dolphin uses the same transaction
  engine for selected external Gecko definitions, including rollback; see
  [`docs/RETROARCH_GUI_APPLY_HISTORY.md`](docs/RETROARCH_GUI_APPLY_HISTORY.md)
  and
  [`docs/SHARED_SAFE_APPLY_ROLLBACK.md`](docs/SHARED_SAFE_APPLY_ROLLBACK.md).
  EmuWiz does not execute cheat files or any other retrieved content at
  any stage of preview, apply, or rollback.
- The Sources page owns RetroArch trusted-catalogue retrieval end-to-end:
  Download when nothing is cached, Update when a snapshot exists, and an
  always-available read-only Verify, each starting only after an explicit
  review-then-confirm dialog naming the exact destination. Catalogue
  download is a separate step from cheat installation - fetching a
  catalogue never installs anything; see
  [`docs/RETROARCH_CHEAT_SOURCES.md`](docs/RETROARCH_CHEAT_SOURCES.md).
- Inventories, verifies, pins and deliberately prunes immutable cheat-source
  snapshots with preview-first cache maintenance. Current, last-known-good and
  pinned snapshots remain protected, and retrieval and maintenance coordinate
  through one bounded cross-process cache lock; see
  [`docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md`](docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md).
- Shows **Recently Found**: a dedicated navigation page listing only the
  newest completed scan's newly added archives, in exact path order,
  persisted across restarts and bounded to 10,000 entries with explicit
  truncation reporting; see
  [`docs/LIBRARY_SCAN_USABILITY.md`](docs/LIBRARY_SCAN_USABILITY.md).
- Recognizes loose **Mega Drive/Genesis** ROMs: `.gen`/`.smd` files
  case-insensitively, and ambiguous `.md`/`.bin` files only when they sit
  under an exactly-named Mega Drive/Genesis folder component - never from
  the filename alone - so an unrelated `README.md` elsewhere is never
  imported; see
  [`docs/LIBRARY_SCAN_USABILITY.md`](docs/LIBRARY_SCAN_USABILITY.md).
- Builds a JSON index and watches source folders to keep it fresh, without ever auto-mounting or auto-unmounting.
- Includes config validation and Doctor Stages 1A/1B/1C-A: read-only setup,
  environment, storage, emulator-profile, and managed-cheat findings; narrowly
  bound confirmed repairs where safe; and grouped historical mount results.
- Offers the optional **BSFree Archive** under Cheats → Sources. Download or
  local import is explicit; the historical third-party SQLite database is
  validated, stored immutably, and queried read-only/query-only with bounded
  pagination. BSFree Stage 1 is browse-only and has no Install action.
- Supports an optional **RomM** server as an external identity source
  (`identity source romm ...`, and Sources → RomM in the GUI): configuration,
  bounded catalogue import, browsing, per-archive identity matching, and
  cover artwork, all read-only towards RomM - no command writes to it,
  triggers a scan on it, or touches a ROM. Only loopback and private LAN
  addresses are accepted, and the access token is never printed, logged, or
  stored in config or cache JSON.
- Ships a desktop GUI (`emuwiz`) covering scanning, mounting, sources
  (including RomM and DAT/Cheat catalogue management), library views,
  duplicates, catalogue health, Cheats & Mods, Doctor, History & Logs, and
  Settings over the same core logic as the CLI.
- Provides stable, documented JSON output for several commands - see [`docs/json-api.md`](docs/json-api.md).

## Current limitations

- No automatic patch or cheat installation. Supported RetroArch, Dolphin,
  PCSX2, GameCube, Wii, and Xenia flows require an exact preview and explicit
  confirmation, then use the verified transaction/History/Undo path. BSFree is
  browse-only, and unsupported or ambiguous formats remain non-installable.
- No broad multi-emulator support yet - PCSX2, RetroArch, Dolphin, and Xenia
  are the only emulators with patch/cheat workflows today, and EmuWiz never
  launches an emulator.
- Not every archive format, Linux distribution, emulator, or frontend is
  supported or tested - see [Supported/tested environments](#supportedtested-environments-and-formats).
- No automatic modification of emulator configuration files.
- No official distribution of games, ROMs, BIOS, firmware, or patches -
  EmuWiz organizes and previews collections you already have.
- This is alpha software: workflows may be incomplete, and defects should
  be expected. See [`CHANGELOG.md`](CHANGELOG.md) for what has actually
  shipped.
- ZIP-contained identity, CHD, RVZ, and generic BIN identity remain incomplete
  for some platforms/layouts.

## Install from a Release

Prebuilt Linux binaries are published on the [Releases](https://github.com/kiehntre/archivefs/releases) page for tagged versions, for example `v0.5.0-alpha`. This is the quickest way to get running without building from source.

1. Download the release tarball and its `SHA256SUMS` file, for example:

   ```sh
   curl -LO https://github.com/kiehntre/archivefs/releases/download/v0.5.0-alpha/archivefs-v0.5.0-alpha-x86_64-linux.tar.gz
   curl -LO https://github.com/kiehntre/archivefs/releases/download/v0.5.0-alpha/SHA256SUMS
   ```

2. Verify the tarball against the checksum file before extracting it:

   ```sh
   sha256sum -c SHA256SUMS --ignore-missing
   ```

3. Extract it:

   ```sh
   tar -xzf archivefs-v0.5.0-alpha-x86_64-linux.tar.gz
   cd archivefs-v0.5.0-alpha-x86_64-linux
   ```

### Quick install

From inside the extracted directory, run the installer:

```sh
./install.sh
```

This installs `emuwiz-cli` and `emuwiz` into `~/.local/bin` (override the location with `--prefix PATH`), creates `~/.config/emuwiz`, and copies `config.toml.example` to `config.toml` there - but only if a config does not already exist; an existing config is never touched. An existing `~/.config/archivefs` from before the rename is still honoured, so pre-rename settings keep loading. The `emuwiz-gui` alias and legacy `archivefs-cli`/`archivefs-gui` names are installed too. It uses no `sudo` and does not modify your shell startup files. It also checks whether `ratarmount` is on `PATH` and prints installation guidance if it is not. It is safe to run again later (for example after upgrading to a newer release tarball).

Edit `source_folders` and `mount_root` in `~/.config/emuwiz/config.toml`, then run `emuwiz-cli doctor` (see the PATH note below if that command is not found).

To remove what it installed (your config is left in place):

```sh
./install.sh --uninstall
```

Pass the same `--prefix PATH` to `--uninstall` if you installed to a non-default location. Run `./install.sh --help` for the full list of options.

**PATH note:** the installer never edits shell startup files, so if `~/.local/bin` is not already on your `PATH`, add it yourself - for example add this line to `~/.bashrc` or `~/.zshrc`, then restart your shell (or `source` that file):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Until then, run the installed binaries with their full path: `~/.local/bin/emuwiz-cli doctor`.

### Manual installation

Manual installation remains available if you would rather control each step yourself, or need to install somewhere the script does not handle:

4. Make the binaries executable, if extraction did not already preserve that:

   ```sh
   chmod +x emuwiz-cli emuwiz
   ```

5. Install `ratarmount` separately. It is an external dependency that EmuWiz shells out to for mounting - it is not bundled in the release tarball, and archive mounting will not work without it. Install it however fits your system, then make sure the `ratarmount` command is on your `PATH` (or point `ratarmount_bin` in the config at its full path).

6. Copy the example configuration and edit it for your system:

   ```sh
   mkdir -p ~/.config/emuwiz
   cp config.toml.example ~/.config/emuwiz/config.toml
   ```

   Edit `source_folders` and `mount_root` in `~/.config/emuwiz/config.toml` to point at real paths on your machine.

7. Check that everything is set up correctly:

   ```sh
   ./emuwiz-cli doctor
   ./emuwiz-cli config-check
   ```

8. Launch the desktop GUI, if you want it:

   ```sh
   ./emuwiz
   ```

   `emuwiz` needs a running Linux desktop session (X11 or Wayland) with the usual runtime graphics libraries present - it will not open a window over a bare SSH session or on a headless server with no desktop environment.

Archive mounts created by EmuWiz are always read-only; it never modifies files in your configured `source_folders`.

### Upgrade from v0.6 development builds

Stop EmuWiz and back up `~/.local/share/archivefs/library.sqlite3` and
managed cheat/history state. Installing the new binaries preserves existing
configuration. The v0.7 candidate migrates the catalogue forward through
schema 6 (migrations `0001`–`0006`); an older binary cannot open that database,
so rollback requires restoring the backup rather than editing SQLite metadata.
After upgrading, run `emuwiz-cli doctor --findings` and rescan sources whose
stored platform findings predate the canonical registry.

There is currently no package-manager distribution of EmuWiz (no apt, dnf, pacman, Homebrew, or similar package) - the release tarball above and building from source below are the two supported ways to install it.

## Supported/tested environments and formats

- **Platform:** Linux only. Mount and watcher behavior rely on Linux
  facilities (`/proc/self/mountinfo`, FUSE-style mount tools, `inotify` via
  the `notify` crate). macOS and Windows are not supported.
- **Archive formats:** `.zip`, `.7z`, and `.rar` (with split-RAR
  continuation-part skipping).
- **Direct game images:** `.iso`, `.gcm`, `.gcz`, `.rvz`, `.wbfs`, and
  `.ciso` for GameCube/Wii, Atari ST `.st`/`.stx`, plus existing
  platform-specific loose-image formats. Direct images are library items;
  archive mounting remains limited to the archive formats above.
- **Mount backend:** `ratarmount` only, invoked as an external tool - not
  bundled, must be installed and on `PATH` separately.
- **Desktop GUI:** requires a running X11 or Wayland session; there is no
  headless mode.
- This list reflects what the code and tests currently exercise, not an
  exhaustive compatibility guarantee across every Linux distribution.

## Build from Source (Developers)

EmuWiz is a Rust workspace. It pins an exact Rust toolchain via
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

EmuWiz also includes a desktop frontend built with `egui`/`eframe`. It scans in the background and shows archive totals, mount states, doctor checks, paths, platforms, sources, catalogue duplicates and health, and searchable status rows, with the same read-only-by-default safety model as the CLI.

Build and run it from the workspace root:

```sh
cargo build -p archivefs-gui
cargo run -p archivefs-gui
```

The GUI uses the same `~/.config/emuwiz/config.toml` configuration and core scanning/catalogue logic as the CLI. Use **Refresh** to rescan after filesystem or mount-state changes.

Archive mounting uses `ratarmount`, so install it separately and make sure it is available on `PATH`, or set `ratarmount_bin` in the config.

## Configuration

EmuWiz reads its default config from `~/.config/emuwiz/config.toml` (an existing
`~/.config/archivefs` is still honoured from before the rename).

Example:

```toml
source_folders = ["/data/archives"]
mount_root = "/mnt/archivefs"
ratarmount_bin = "ratarmount"
```

The same example, with comments, ships as [`config.toml.example`](config.toml.example) in this repository and in every release tarball - copy it to `~/.config/emuwiz/config.toml` as a starting point.

`source_folders` are scanned recursively. `mount_root` is where EmuWiz creates planned mount directories. EmuWiz does not modify files in `source_folders`. `ratarmount_bin` is optional and defaults to `"ratarmount"` resolved from `PATH`.

**Note on syntax:** EmuWiz uses a small hand-written config parser, not a full TOML implementation. `source_folders = ["/data/archives"]` on one line (shown above) always works. Splitting the array across multiple lines, e.g.:

```toml
source_folders = [
  "/data/archives",
  "/data/more-archives",
]
```

is also accepted, but only this `key = "value"` / `key = [...]` form is understood - there is no support for TOML tables, inline tables, or nested arrays.

Managed Library Views and persistent multi-source configuration use their
own JSON files under `~/.config/emuwiz/` (`library_views.json`,
`sources.json`) - see [`docs/library-views.md`](docs/library-views.md).

## Common Commands

Scanning, status, and mounting:

```sh
emuwiz-cli doctor
emuwiz-cli config-check
emuwiz-cli scan
emuwiz-cli status
emuwiz-cli stats
emuwiz-cli info "007 Legends"
emuwiz-cli mount-one "007 Legends"
emuwiz-cli unmount-one "007 Legends"
emuwiz-cli duplicates
emuwiz-cli index-build
emuwiz-cli index-show
emuwiz-cli index-find "xbox360"
emuwiz-cli watch
```

Persistent catalogue and multi-source management:

```sh
emuwiz-cli library-status
emuwiz-cli database-check
emuwiz-cli database-check --json
emuwiz-cli library-scan
emuwiz-cli library-list
emuwiz-cli library-find "007 Legends"
emuwiz-cli library-set-platform "Luigi's Mansion" GameCube
emuwiz-cli platform-alias-add gc GameCube
emuwiz-cli platform-detect /data/roms/atarist/game.st
emuwiz-cli sources
emuwiz-cli source add /data/more-archives
emuwiz-cli source scan-all
```

Managed library views:

```sh
emuwiz-cli view list
emuwiz-cli view preview "By Platform"
emuwiz-cli view apply "By Platform"
```

Patch preview:

```sh
emuwiz-cli pcsx2-patch-preview
emuwiz-cli pcsx2-patch-preview --json
```

RetroArch environment discovery:

```sh
emuwiz-cli retroarch-environment
emuwiz-cli retroarch-environment --json
```

RetroArch cheat/patch destination preview:

```sh
emuwiz-cli retroarch-patch-preview
emuwiz-cli retroarch-patch-preview --json
```

RetroArch cheat installation history:

```sh
emuwiz-cli retroarch-cheat-history
emuwiz-cli retroarch-cheat-history --json
emuwiz-cli retroarch-cheat-inspect ~/.local/share/archivefs/cheat-install-runs/<run>.json
```

Optional BSFree browse-only source:

```sh
emuwiz-cli cheats source bsfree status --json
emuwiz-cli cheats source bsfree import-local /path/to/bsfree.db
emuwiz-cli cheats source bsfree search --platform NES --title MARIO --json
```

The import/download commands are explicit. Status, search, game browsing, and
ordinary GUI browsing do not perform network access or write emulator files.

Use verbose or debug logging when you need more detail:

```sh
emuwiz-cli --verbose stats
emuwiz-cli --debug watch
```

Run `emuwiz-cli --help` for the complete, current command list with
descriptions.

## Typical Workflow

1. Create `~/.config/emuwiz/config.toml` (or keep using an existing pre-rename `~/.config/archivefs/config.toml`).
2. Run `emuwiz-cli config-check` to validate the config.
3. Run `emuwiz-cli doctor` to check source folders, mount root, tools, and current archive state.
4. Run `emuwiz-cli library-scan` to build the persistent catalogue, then `emuwiz-cli stats` or `emuwiz-cli library-list` to inspect what EmuWiz sees.
5. Run `emuwiz-cli info "name"` to inspect one archive.
6. Run `emuwiz-cli mount-one "name"` to mount a single archive.
7. Run `emuwiz-cli unmount-one "name"` when finished.
8. Optionally set up a Library View (`emuwiz-cli view preview`/`apply`) for an organized, browsable directory tree.
9. Run `emuwiz-cli watch` if you want EmuWiz to refresh the JSON index when source folders change.

## Example Output

`emuwiz-cli stats`:

```text
EmuWiz Stats

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

`emuwiz-cli info "007 Legends"`:

```text
EmuWiz Info

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

`emuwiz-cli index-show`:

```text
EmuWiz Index

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
- [v0.7.0 release notes](docs/releases/v0.7.0.md)
- [v0.7.0-rc.1 historical candidate notes](docs/releases/v0.7.0-rc.1.md)
- [Domain model](docs/domain-model.md)
- [Persistent database](docs/database.md) / [database design](docs/DATABASE_DESIGN.md) / [ADR 0001](docs/adr/0001-persistent-library-database.md)
- [Managed library views](docs/library-views.md)
- [Patch & cheat manager design (PCSX2 preview, adapter boundary)](docs/PATCH_CHEAT_MANAGER_DESIGN.md)
- [Read-only PCSX2 Cheats & Mods adapter](docs/PCSX2_READONLY_ADAPTER.md)
- [Read-only Dolphin Cheats & Mods adapter](docs/DOLPHIN_READONLY_ADAPTER.md)
- [Dolphin and RetroArch cheat-provider coverage](docs/CHEAT_PROVIDER_COVERAGE.md)
- [Shared verified game identity](docs/SHARED_GAME_IDENTITY.md)
- [Shared read-only Cheats & Mods preview](docs/SHARED_CHEAT_PREVIEW.md)
- [Shared safe apply, journal, and rollback foundation](docs/SHARED_SAFE_APPLY_ROLLBACK.md)
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
- [Historical untagged v0.6 development notes](docs/RELEASE_NOTES_v0.6.0-alpha.md)
- [Historical v0.6 manual QA plan](docs/MANUAL_QA_v0.6.0-alpha.md)
- [v0.5.0-alpha release notes](docs/RELEASE_NOTES_v0.5.0-alpha.md)
- [v0.5.0-alpha manual QA plan](docs/MANUAL_QA_v0.5.0-alpha.md)
- [Adapter support matrix](docs/ADAPTER_SUPPORT_MATRIX.md)
- [Release checklist](docs/release-checklist.md)
- [Paper cuts / small usability notes](docs/paper-cuts.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy / reporting](SECURITY.md)
- [Vision](VISION.md)

## Dedication

EmuWiz is dedicated to [my dad](DEDICATION.md).
