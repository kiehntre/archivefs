# Shared read-only Cheats & Mods preview

ArchiveFS has a shared source-to-destination preview and conflict model for the
RetroArch, PCSX2, and Dolphin Cheats & Mods workflows. It is observational:

> Preview only. No files were changed.

There is no apply path in this milestone. Previewing cannot install, replace,
enable, disable, delete, back up, journal, or roll back content.

## Preview and destination states

The typed preview state is one of: `Install new`, `Already installed`, `Replace
different`, `Conflict`, `Ambiguous`, `Not eligible`, `Unsupported`, `Unsafe
destination`, `Destination unavailable`, `Source unavailable`, `Identity
unavailable`, or `Resource limit reached`.

The separately reported destination state distinguishes missing, regular and
identical, regular and different, directory, symlink, special file,
inaccessible, changed during inspection, and unavailable. Proposed actions are
only `Install`, `Skip`, `Replace`, or `Blocked` metadata. No proposed action is
executable.

Each entry preserves the adapter, exact selected archive path, verified
identity when present, match strength, exact source path and SHA-256,
destination root/relative/final paths, existing destination SHA-256, typed
blockers and warnings, and the future backup/replacement-permission flags.
Core destination-safety errors remain typed rather than becoming prose.

## Eligibility

PCSX2 requires a verified executable CRC and one exact PNACH match. Dolphin
requires a verified Game ID; Game-ID-only and revision-aware matches remain
distinct. Candidate filename/title evidence is visible but blocked. Multiple
exact matches are conflicts.

RetroArch preserves its existing exact/strong/candidate/ambiguous/unsupported
strength vocabulary. The shared engine accepts exact or strong RetroArch input
only alongside verified selected-archive identity; weak/candidate-only input is
never eligible. The current GUI does not yet materialize a selected trusted
catalogue's individual RetroArch staging records into this shared report, so it
truthfully reports source unavailable instead of inventing a source item.

PCSX2 destinations are accepted only when an inspected matched file maps back
to exactly `<configuration>/<category>/<filename>`. Deeper recursive PNACH
paths remain blocked conservatively. Dolphin maps only
`<configuration>/GameSettings/<filename>`. No Dolphin texture-pack preview is
implemented.

## Destination safety

The preview reuses ArchiveFS's destination-safety validator. It rejects
relative roots, filesystem roots, traversal or embedded-separator components,
paths outside the approved root, root/parent/final symlinks, non-directory
parents, directories or special files at the final path, inaccessible paths,
and identities that change while being read. Missing roots or parents are
reported; preview never creates them.

Duplicate final destinations, filename collisions, conservative case-folded
collisions, one source targeting several destinations, multiple exact sources,
duplicate source content, adapter/platform mismatches, stale identity, source
changes, and destination changes are retained as typed conflicts. None is
silently resolved.

## Source revalidation and hashing

Every source path must be absolute, regular, and free of symlink components.
It is opened read-only with `O_NOFOLLOW` on Unix. Device/inode, size, and
modification time are compared before and after reading. The newly calculated
digest is compared with the digest recorded by the adapter inventory; a
mismatch is `Source changed`. Destination regular files receive the same
bounded, race-aware treatment. Duplicate paths are hashed once per report.

## Deterministic limits

| Resource | Limit |
|---|---:|
| Preview entries | 512 |
| Unique source files hashed | 256 |
| Unique destination files hashed | 256 |
| Bytes hashed per file | 1 MiB |
| Total source and destination bytes hashed | 32 MiB |
| Destination paths inspected | 1,024 |
| Conflict records retained | 128 |
| Warnings retained per entry/report | 64 |

Limit exhaustion produces an explicit incomplete/resource-limited report.
Entries, blockers, conflicts, and summary counts are sorted and calculated
deterministically.

## GUI and stale-result behavior

The shared Preview section uses summary and entry cards with expandable
technical details. Work runs on a background thread. A result applies only
while exact archive, adapter, profile, source mode, selected source, page, and
platform context still match. Identity or adapter-inventory reinspection also
invalidates the prior preview. Preview activity records one start and one
completion, blocked/conflict, or failure event—not one event per file.

## Read-only and future work

Production preview code creates no files or directories, writes no temporary
data, renames and deletes nothing, executes and mounts nothing, and performs no
network operation or metadata upload. Synthetic fixture writes exist only in
unit tests.

A future apply milestone would need a fresh immediately-before-write safety
check, explicit replacement consent, verified backups, durable journaling,
atomic installation where supported, post-write verification, and tested
rollback. None of those capabilities or controls exists here.
