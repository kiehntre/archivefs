# ArchiveFS Security

ArchiveFS treats all downloaded archives as untrusted.

## Core rules

- Never write inside source archives.
- Never extract archives by default.
- Never mount outside the configured `mount_root`.
- Never follow paths that escape the mount root.
- Never invoke shell commands with interpolated strings.
- Always pass subprocess arguments directly.
- Prefer read-only mounts.
- Never delete source data automatically.
- Retry failed mounts safely, with limits.

## Threat model

ArchiveFS may process archives from the internet, download tools, debrid services, and user collections. These archives may be corrupt, malicious, incomplete, or intentionally crafted to abuse path handling.

## Path traversal

ArchiveFS must reject archive entries such as:

- `../../file`
- `/absolute/path`
- paths containing unsafe symlink escapes

## Mount containment

All mount paths must stay under the configured `mount_root`.

## Symlinks

Symlinks inside archives must not be followed if they escape the mount root or expose host paths.

## Command execution

ArchiveFS must not use shell string execution. External tools such as `ratarmount` must be invoked with direct argument arrays.

## Read-only by default

Mounted archives should be read-only unless a future user explicitly enables writable overlays.

## Destructive actions

ArchiveFS must not delete, modify, or overwrite source archives automatically.

## Retry behaviour

Failed mounts may be retried manually or automatically when source files change, but retry loops must have limits and backoff.

## Audit logging

ArchiveFS should log scans, mounts, unmounts, retries, failures, and security warnings.

## GUI warnings

The GUI should clearly warn when an archive is corrupt, incomplete, unsafe, or blocked by security rules.
