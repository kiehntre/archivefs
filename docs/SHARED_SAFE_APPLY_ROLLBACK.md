# Shared safe apply, journal, and rollback foundation

ArchiveFS has a bounded transaction pipeline for exact file-oriented Cheats &
Mods entries. It is not a general mod installer. Execution is available only
for an eligible shared preview backed by a separately verified identity, an
adapter-approved materialized local source, and an exact safe destination.

## Adapter support

| Adapter | Materialized source | Exact identity | Destination | Preview | Shared apply / rollback |
|---|---|---|---|---|---|
| RetroArch trusted catalogue | Available in the existing immutable catalogue snapshot | Exact or strong trusted catalogue match, bound to verified archive evidence | Canonical `.cht` path | Available in core | **Available and wired into the GUI** through the per-game catalogue materialization bridge; see [`RETROARCH_GUI_APPLY_HISTORY.md`](RETROARCH_GUI_APPLY_HISTORY.md) |
| PCSX2 | Unavailable; current adapter inventories emulator-managed PNACH files only | Verified executable CRC | Exact PNACH inventory path | Available | Preview-only: no independent approved source artifact |
| Dolphin | Unavailable; current adapter inventories emulator-managed GameSettings INIs only | Verified Game ID, with revision kept distinct | Exact GameSettings INI inventory path | Available | Preview-only: no independent approved source artifact; no texture packs |

No adapter becomes writable merely because a destination exists.

## Transaction and confirmation

The typed stages cover dry run, install, replacement, already installed,
eligibility/conflict/replacement skips, source or destination changes, backup,
write, verification, journal, complete/partial failure, and rollback states.
Each entry records schema and operation identity, selected archive and verified
game identity, exact source/destination paths and digests, destination pre-state,
replacement approval, backup and temporary paths, created directories, final
digest, verification, warnings, and typed failures.

Dry run is the default whenever general confirmation is absent. Confirmation is
bound to the SHA-256 plan ID, which covers exact adapter, archive, identity,
profile, source mode, source root, destination root, paths, digests, states, and
actions. Replacing different content additionally requires a separate
non-preselected permission. Context or plan changes fail closed; the executor
does not silently rebuild a plan.

## Atomic writes and backups

The executor reopens sources no-follow, rejects symlink components and special
files, compares device/inode/size/mtime around bounded reads, and verifies the
approved SHA-256. It re-runs destination safety and checks the approved state and
digest under an exclusive destination-root advisory lock.

Install data is written with exclusive creation to a unique temporary file in
the final directory, flushed, assigned mode `0600` on Unix, verified, and
atomically renamed without replacement. The final file is reopened and
verified, and the parent directory is flushed where supported. An approved
missing RetroArch platform directory may be created one component at a time and
is journalled.

Replacement first creates a verified, never-overwritten backup under:

```text
<ArchiveFS managed backup root>/<operation ID>/<destination digest>.bak
```

Only then is the verified temporary file atomically renamed over the freshly
revalidated destination. Backups are retained indefinitely in this milestone;
there is no automatic pruning.

## Journal and partial success

One schema-version-1 JSON journal is atomically written with exclusive final
creation under the ArchiveFS-managed history root as `<operation ID>.json`.
Operation IDs are validated filename components, duplicate journals are
rejected, and journal serialization is bounded. Journals preserve exact Unix
path bytes in hexadecimal alongside display text.

If the destination changed successfully but journal creation fails, the result
is `partial_failure`, never complete failure. The in-memory result retains the
exact entry state and the serious journal failure. History scanning is local,
read-only, sorted, bounded, and skips malformed or unsupported journals with
warnings rather than crashing.

## Rollback

Rollback begins with a separate read-only preview bound by its own digest. It
checks journal schema/root, current destination digest and type, backup location
and digest, and an existing completion marker. User-modified content, missing or
changed backups, symlinks, special files, root mismatches, and repeated rollback
are blocked.

Confirmed install-new rollback removes only the exact still-matching installed
file. It removes a journal-proven created parent only when still empty and below
the approved root. Replacement rollback atomically restores and verifies the
retained backup without overwriting modified content. A separate
`<original-operation ID>.rollback.json` marker preserves the original evidence
and makes repeated rollback non-destructive.

## Locking and limits

The executor uses an advisory exclusive lock on the open destination-root
directory. Locks create no artifacts, time out explicitly after five seconds,
are released by descriptor close/process termination, and require no stale-lock
cleanup. One transaction has one exact destination root, so lock ordering is
deterministic and deadlock-free.

| Resource | Limit |
|---|---:|
| Apply entries | 128 |
| Source bytes per file | 1 MiB |
| Total destination bytes written | 32 MiB |
| Total backup bytes | 32 MiB |
| Journal size | 2 MiB |
| History journals scanned | 512 |
| Rollback entries | 128 |
| Warnings retained | 64 |
| Failures retained | 128 |
| Created parent directories | 32 |
| Temporary files | 128 |

Limit exhaustion fails closed. Temporary files are cleaned after detected
failures. A post-rename verification failure is reported truthfully and the
replacement backup remains available.

## GUI, privacy, and boundaries

Cheats & Mods presents the six-stage controlled flow and adapter readiness. It
shows no Apply control for candidate, blocked, preview-only, or non-materialized
entries. The GUI bridges a fetched RetroArch catalogue's individual per-game
source into the shared preview through the same reviewed materialized-source
seam the core and existing RetroArch installer already use.

The pipeline performs no download, upload, mount, or process execution. It does
not interpret cheat directives and never launches an emulator, script, binary,
or discovered executable. Writes are limited to explicitly approved destination
files/directories and ArchiveFS-managed backup, history, temporary, and rollback
marker paths.

Future work includes multi-root transactions, cancellation checkpoints after
writing begins, richer activity event projection, and conservative
user-driven backup retention management. The GUI RetroArch materialization
bridge is implemented; see
[`RETROARCH_GUI_APPLY_HISTORY.md`](RETROARCH_GUI_APPLY_HISTORY.md).
