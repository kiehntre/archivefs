# Security Policy

ArchiveFS is alpha-stage software. This document describes its current
safety boundaries and how to report a vulnerability - it is not a security
guarantee or a certification.

## Reporting a vulnerability

If you find a security issue, please open a GitHub issue on this repository
describing the problem, or contact the maintainer directly if the issue is
sensitive enough that you would rather not describe it publicly first.
There is no dedicated security mailing list or bug-bounty program at this
stage.

Please include:

- The affected version or commit.
- Steps to reproduce, or the specific code path you're concerned about.
- What you'd expect to happen instead.

There is currently no formal disclosure timeline commitment, given this is a
small, alpha-stage project - but reports will be looked at and, where
confirmed, fixed and noted in `CHANGELOG.md` under a `Security` entry.

## Current safety boundaries

These are the security-relevant properties ArchiveFS's design and tests
currently rely on. See [`docs/security.md`](docs/security.md) for the full
detail behind each of these.

- **Local-first, no telemetry.** ArchiveFS does not send usage data,
  crash reports, or file information anywhere. The only network access in
  the codebase today is the PCSX2 patch-metadata fetch
  (`pcsx2-patch-preview`), which is an explicit, user-invoked, read-only
  HTTPS request to a single compiled-in metadata endpoint.
- **Archives are treated as untrusted input.** Filenames and archive
  contents may be attacker-controlled; mount-name generation and path
  handling are designed not to let an archive's name or internal paths
  escape the configured mount area.
- **Mounts and unmounts are scoped.** Mounts are only created under the
  configured `mount_root`; unmounts only ever target paths under it.
  Library View destinations may not overlap a configured source folder.
- **Source archives are never modified.** Scanning, mounting, cataloguing,
  duplicate detection, and library-view/patch-preview operations all read
  archive metadata and filesystem state; none of them rewrite a source
  archive file.
- **The persistent catalogue is additive, not authoritative for safety.**
  Mount, unmount, lazy-unmount, and cleanup code paths read live filesystem
  and mount state directly and do not depend on the SQLite catalogue being
  present, complete, or uncorrupted. See
  [ADR 0001](docs/adr/0001-persistent-library-database.md).
- **The PCSX2 patch preview is read-only and non-executable.** It fetches
  metadata into bounded memory and reports installation *candidates* - it
  does not download patch artifacts, write files, or modify emulator
  configuration. See
  [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md)
  for the full trust model that any future artifact-downloading or
  file-installing capability would need to satisfy before being enabled.

## Security-sensitive areas for contributors

If you're changing code in these areas, please be especially careful and
call out the safety implications explicitly in your pull request:

- Path validation and mount-name generation (`archivefs-core`).
- Mount/unmount/cleanup execution and lazy-unmount recovery.
- Source-folder and Library-View destination overlap validation.
- Anything that constructs a filesystem path from archive-supplied or
  remote-supplied data.
- Anything that would add a new outbound network request, a new write path,
  or a new place where downloaded content is trusted.

## Out of scope

ArchiveFS does not currently implement, and reports about the *absence* of
these are expected rather than actionable bugs:

- Malware/virus scanning of archive contents.
- A sandboxed or containerized execution model.
- A GUI-specific permission model beyond the same checks the CLI uses.
