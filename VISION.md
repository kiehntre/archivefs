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
- Provide CLI, daemon, and GUI clients over one shared core library.

## Non-goals for v0.1

- No native Rust FUSE yet.
- No GUI yet.
- No Docker yet.
- No destructive file operations.
