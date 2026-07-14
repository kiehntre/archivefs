<img width="1024" height="559" alt="archivefs-banner" src="https://github.com/user-attachments/assets/aa1816c0-316c-4c1b-986e-ba7c14bae9c5" />

# ArchiveFS

ArchiveFS is a Linux-first command-line tool for mounting archive files as read-only folders. It scans configured source folders, plans safe mount paths, can mount archives through `ratarmount`, and provides status, stats, info, index, and watcher commands over the same core scanner.

ArchiveFS is designed to inspect and mount archives without modifying the original archive files.

## Key Features

- Scans configured folders for supported archives: `.zip`, `.7z`, and `.rar`.
- Skips obvious split RAR continuation parts such as `.r00` and non-primary `.partN.rar` files.
- Mounts archives read-only through `ratarmount`.
- Tracks archive path, mount path, mount state, health, size, modified time, and platform hints.
- Provides `status`, `stats`, and `info` commands for inspecting a library.
- Builds a JSON index for summary and search commands.
- Watches source folders and refreshes the JSON index without auto-mounting or auto-unmounting.
- Includes config validation and doctor diagnostics.

## Install from a Release

Prebuilt Linux binaries are published on the [Releases](https://github.com/kiehntre/archivefs/releases) page for tagged versions, for example `v0.2.0-alpha`. This is the quickest way to get running without building from source.

1. Download the release tarball and its `SHA256SUMS` file, for example:

   ```sh
   curl -LO https://github.com/kiehntre/archivefs/releases/download/v0.2.0-alpha/archivefs-v0.2.0-alpha-x86_64-linux.tar.gz
   curl -LO https://github.com/kiehntre/archivefs/releases/download/v0.2.0-alpha/SHA256SUMS
   ```

2. Verify the tarball against the checksum file before extracting it:

   ```sh
   sha256sum -c SHA256SUMS --ignore-missing
   ```

3. Extract it:

   ```sh
   tar -xzf archivefs-v0.2.0-alpha-x86_64-linux.tar.gz
   cd archivefs-v0.2.0-alpha-x86_64-linux
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

## Build from Source (Developers)

ArchiveFS is a Rust workspace. Build the CLI from source:

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

## Desktop GUI

ArchiveFS also includes a small read-only desktop frontend. It scans in the background and shows archive totals, mount states, doctor checks, paths, platforms, and searchable status rows.

Build and run it from the workspace root:

```sh
cargo build -p archivefs-gui
cargo run -p archivefs-gui
```

The GUI uses the same `~/.config/archivefs/config.toml` configuration and core scanning logic as the CLI. Use **Refresh** to rescan after filesystem or mount-state changes.

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

## Common Commands

```sh
archivefs-cli doctor
archivefs-cli config-check
archivefs-cli scan
archivefs-cli status
archivefs-cli stats
archivefs-cli info "007 Legends"
archivefs-cli mount-one "007 Legends"
archivefs-cli unmount-one "007 Legends"
archivefs-cli index-build
archivefs-cli index-show
archivefs-cli index-find "xbox360"
archivefs-cli watch
```

Use verbose or debug logging when you need more detail:

```sh
archivefs-cli --verbose stats
archivefs-cli --debug watch
```

## Typical Workflow

1. Create `~/.config/archivefs/config.toml`.
2. Run `archivefs-cli config-check` to validate the config.
3. Run `archivefs-cli doctor` to check source folders, mount root, tools, and current archive state.
4. Run `archivefs-cli stats` or `archivefs-cli scan` to inspect what ArchiveFS sees.
5. Run `archivefs-cli info "name"` to inspect one archive.
6. Run `archivefs-cli mount-one "name"` to mount a single archive.
7. Run `archivefs-cli unmount-one "name"` when finished.
8. Run `archivefs-cli index-build` and `archivefs-cli index-find "query"` for indexed search.
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

Developer and design docs live in [`docs/`](docs/):

- [Architecture](docs/architecture.md)
- [Roadmap](docs/roadmap.md)
- [Domain model](docs/domain-model.md)
- [Watcher](docs/watcher.md)
- [Security](docs/security.md)
- [Database notes](docs/database.md)
- [Provider pipeline](docs/provider-pipeline.md)
