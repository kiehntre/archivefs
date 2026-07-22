# ArchiveFS Security Design

This document describes the security boundaries and safety rules for ArchiveFS.

ArchiveFS mounts untrusted archive files as folders. Archives may be incomplete, corrupt, malicious, or unexpectedly large. The security design should assume archive contents and filenames are attacker-controlled.

## Goals

- Never perform destructive operations on source archives.
- Only mount archives discovered from configured source folders.
- Only unmount mountpoints created under the configured `mount_root`.
- Avoid path traversal through archive filenames, internal paths, or generated mount names.
- Keep mount backend behavior isolated behind `MountBackend`.
- Make failures visible and retryable without hiding unsafe state.

## Non-Goals

- No native FUSE implementation - `ratarmount` remains the mount backend.
- There is no separate daemon process, so there is no daemon-specific
  security model to design.
- No formal GUI permission model beyond the same config-identity and
  mount/unmount safety checks the CLI uses.
- No Docker or container sandbox design.
- No malware scanning.

## Trust Boundaries

### Config File

ArchiveFS reads configuration from:

```text
~/.config/archivefs/config.toml
```

The config controls source folders, mount root, and the ratarmount binary path. ArchiveFS should treat config as user-controlled but not archive-controlled.

Security rules:

- Source folders must be explicit absolute non-root paths. Scans reject duplicate or
  nested roots and refuse symlink components; symlink entries encountered below a
  valid root are never followed.
- Recursive scans are deterministic and bounded by entry and depth limits. They skip
  special files and revalidate source/archive filesystem identity before catalogue
  persistence.
- Mounts must be created under the configured `mount_root`.
- Unmount operations must never target paths outside `mount_root`.

### Source Archives

Archive files are untrusted input.

Security rules:

- Do not modify or delete source archives.
- Do not extract archives into source folders.
- Do not trust archive filenames as safe path components.
- Skip obvious split archive continuation parts to avoid mounting incomplete fragments.
- Mark unsupported, corrupt, missing-part, and permission failures in archive health instead of guessing.
- Catalogue refreshes use one SQLite write transaction with per-source savepoints, so
  a failed source cannot leave partial rows and a fatal refresh cannot expose a
  half-updated catalogue.

### Mount Root

The mount root is the only place ArchiveFS should create mount directories.

Security rules:

- Generate safe mount names from archive names.
- Resolve duplicate mount names deterministically.
- Treat pre-existing mount directories as potentially suspicious unless they are confirmed mounted by ArchiveFS.
- Only unmount paths under `mount_root`.
- Prefer unmounting known mounted paths from system mount information, filtered by `mount_root`.

### Mount Backend

ArchiveFS currently uses `ratarmount` through `RatarmountBackend`.

Security rules:

- Core mount logic should depend on the `MountBackend` trait.
- Backend implementations should receive a `MountPlan`, not raw unvalidated strings.
- Backend command arguments should be passed as arguments, not shell-concatenated command strings.
- Backend failures should be surfaced as health or command errors.

## Path Safety

ArchiveFS must not allow archive names or internal archive paths to escape the configured mount area.

Required behavior:

- Convert unsafe filename characters to safe mount-name characters.
- Collapse repeated separators where practical.
- Trim unsafe leading and trailing separators.
- Fall back to a neutral name such as `archive` when a filename has no safe characters.
- Never use archive-internal paths to create host filesystem paths outside a mounted archive view.

## Health and Retry Safety

Archive health exists to make unsafe or incomplete states explicit.

Important states:

- `Failed`: a mount or inspection operation failed.
- `MissingParts`: a split archive appears incomplete.
- `Corrupt`: the archive appears damaged.
- `Unsupported`: the archive format or layout is unsupported.
- `PermissionDenied`: ArchiveFS cannot read or mount the archive.
- `RetryAvailable`: a failed archive can be retried.

Retry behavior should be explicit. ArchiveFS should not silently retry in a tight loop or hide repeated failures.

## Future Work

Future versions should consider:

- Persistent ownership records for mountpoints in SQLite.
- Stronger source-root canonicalization.
- Symlink and bind-mount checks around `mount_root`.
- Separate health diagnostics for missing split archive parts.
- Optional archive hashing before mount.
- Permission checks before invoking mount backends.
- Daemon-specific least-privilege rules.
- GUI warnings for corrupt, unsupported, or permission-denied archives.

## Summary

The current security posture is conservative:

- Archives are mounted read-only through ratarmount.
- Source archives are not modified.
- Mount directories are generated under `mount_root`.
- Unmounting is restricted to paths under `mount_root`.
- The persistent catalogue, managed library views, and the read-only
  PCSX2 patch-preview feature all follow the same rule: they read or
  organize existing state, and none of them is a dependency of mount or
  unmount safety (see [ADR 0001](adr/0001-persistent-library-database.md)).
- Native FUSE and Docker packaging remain out of scope. A desktop GUI now
  exists and reuses the same core safety checks as the CLI rather than its
  own permission model.
