# ArchiveFS Vision

ArchiveFS makes archives behave like normal folders.

It is a virtual filesystem layer for people with large digital libraries, ROM collections, media archives, preservation sets, and download folders.

## Core idea

Archives become folders.

A file like:

    Resident Evil 2.zip

appears as:

    Resident Evil 2/

without extracting it and without using extra disk space.

## Goals

- Mount ZIP, 7Z, and RAR archives as folders.
- Watch source folders automatically.
- Retry failed mounts when archives change.
- Detect corrupt or incomplete archives.
- Avoid false duplicate detection by using platform/system context.
- Provide CLI and GUI clients over one shared core library, and let
  managed archived collections be organized into browsable views without
  extracting them.
- Preview, rather than silently perform, emulator-adapter-specific work
  such as patch installation - see [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md).

## Non-goals

- No native Rust FUSE backend yet - `ratarmount` remains the mount
  backend.
- No Docker packaging.
- No destructive file operations on source archives, ever.
- Not a ROM, BIOS, firmware, or game download service; not a storefront;
  not a DRM system; not a mandatory cloud service; not a telemetry
  platform. See [`ROADMAP.md`](ROADMAP.md#explicitly-out-of-scope-for-now)
  for the full list of what ArchiveFS deliberately will not become.
