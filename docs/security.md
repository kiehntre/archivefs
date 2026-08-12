# EmuWiz Security Design

This document describes the security boundaries and safety rules for EmuWiz.

EmuWiz mounts untrusted archive files as folders, and separately offers a
small number of explicitly gated features that write to the filesystem or
fetch data over the network (DAT-driven rename, cheat/patch installation,
and identity/artwork providers). Archives may be incomplete, corrupt,
malicious, or unexpectedly large. The security design should assume archive
contents and filenames are attacker-controlled, and that every write path
must be either read-only by construction or explicit, previewed, and gated.

## Goals

- Never mutate a source archive, ROM, or disc image as part of mounting,
  inspection, cataloguing, or identity resolution.
- Where a feature does intentionally rename or otherwise write files (DAT
  rename/apply; explicit cheat/patch installers), require an explicit,
  previewed, user-approved action, and make the operation safe to interrupt,
  audit, and undo.
- Only mount archives discovered from configured source folders.
- Only unmount mountpoints created under the configured `mount_root`.
- Avoid path traversal through archive filenames, internal paths, or generated mount names.
- Keep mount backend behavior isolated behind `MountBackend`.
- Make failures visible and retryable without hiding unsafe state.
- Treat network access to third-party providers (RomM, RetroArch, Dolphin,
  Xenia, GameHacking, BSFree, and similar) as explicit and attributable, never
  automatic or silent.

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

EmuWiz reads configuration from its own XDG paths first, and transparently
falls back to the legacy ArchiveFS paths when only they exist (no data is
ever copied, moved, or overwritten by this resolution - see
`crates/archivefs-core/src/app_dirs.rs`):

```text
~/.config/emuwiz/config.toml           (preferred; used if present)
~/.config/archivefs/config.toml        (legacy fallback; used if the EmuWiz
                                         path does not exist yet)
```

The same EmuWiz-first-with-legacy-fallback rule applies to the data directory
(`~/.local/share/emuwiz`, falling back to `~/.local/share/archivefs`) that
holds the catalogue database, DAT rename journals, and other managed state.

The config controls source folders, mount root, and the ratarmount binary path. EmuWiz should treat config as user-controlled but not archive-controlled.

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

Security rules for mounting, inspection, and cataloguing:

- Mounting, signature-based platform detection, disc/archive identity
  inspection, and catalogue refresh never modify or delete a source archive,
  ROM, or disc image. These paths are read-only by construction.
- Do not extract archives into source folders.
- Do not trust archive filenames as safe path components.
- Skip obvious split archive continuation parts to avoid mounting incomplete fragments.
- Mark unsupported, corrupt, missing-part, and permission failures in archive health instead of guessing.
- Catalogue refreshes use one SQLite write transaction with per-source savepoints, so
  a failed source cannot leave partial rows and a fatal refresh cannot expose a
  half-updated catalogue.

#### DAT-driven rename is a separate, explicitly gated mutation path

Unlike mounting and inspection, EmuWiz's DAT rename feature **does** rename
source files on disk once a user explicitly reviews and approves a plan. This
is intentional, and is designed as a narrow, auditable exception to the
read-only rule above rather than a silent one:

- **Planning is read-only.** `crates/archivefs-core/src/dat/rename_plan/`
  builds a proposed rename plan from an audit and the effective matching
  policy without renaming anything; collisions and unsafe (symlink/special)
  sources are flagged in the plan, not acted on.
- **Apply requires explicit, per-item approval.** Only proposals a user has
  marked `AcceptedForReview` are ever included in a transaction
  (`is_approved` / `build_transaction` in
  `crates/archivefs-core/src/dat/rename_apply/executor.rs`).
- **Stale-plan and stale-classifier gates.** Both `build_transaction` and
  `apply_transaction` reject a plan whose generation or classifier version
  does not match the current state (`ApplyError::StalePlan`,
  `ApplyError::StaleClassifierVersion`) before any journal write or file
  mutation occurs, so classification-rule changes since the plan was built
  can never be silently applied.
- **Preflight runs twice.** The whole batch is preflighted before the first
  mutation, and each entry is preflighted again immediately before its own
  rename, so a change that happens between review and apply (a different
  inode, a symlink substitution, an appearing destination) is caught rather
  than blindly executed.
- **The journal is durable and written before mutation.** Every transaction
  is journaled to disk before the first rename, and each entry's state is
  checkpointed to `Applying` before its rename syscall runs, so a crash
  mid-batch leaves a recoverable, inspectable record rather than silent data
  loss (`write_journal`, journal-backed recovery/reconciliation in
  `crates/archivefs-core/src/dat/rename_apply/reconcile.rs`).
- **Renames are no-clobber and filesystem-confirmed.** `rename_noreplace`
  never overwrites an existing destination, and after each rename the
  executor confirms the source is gone and the destination exists with the
  identity captured at review time before marking the entry `Applied`
  (`confirm_rename` in `executor.rs`).
- **Rollback is supported.** Applied transactions can be rolled back through
  `crates/archivefs-core/src/dat/rename_apply/rollback.rs`, including
  journals from before the classifier-version enforcement change, which
  remain inspectable and recoverable.

Outside of this explicitly gated apply path, the "do not modify or delete
source archives" rule above still holds: nothing else in EmuWiz renames,
extracts into, or deletes a source archive, ROM, or disc image.

### Mount Root

The mount root is the only place EmuWiz should create mount directories.

Security rules:

- Generate safe mount names from archive names.
- Resolve duplicate mount names deterministically.
- Treat pre-existing mount directories as potentially suspicious unless they are confirmed mounted by EmuWiz.
- Only unmount paths under `mount_root`.
- Prefer unmounting known mounted paths from system mount information, filtered by `mount_root`.

### Mount Backend

EmuWiz currently uses `ratarmount` through `RatarmountBackend`.

Security rules:

- Core mount logic should depend on the `MountBackend` trait.
- Backend implementations should receive a `MountPlan`, not raw unvalidated strings.
- Backend command arguments should be passed as arguments, not shell-concatenated command strings.
- Backend failures should be surfaced as health or command errors.

### Providers and Emulator Profiles

EmuWiz includes several identity and cheat/patch providers that reach the
network and, for a narrow set of emulator adapters, can write to emulator
configuration after explicit confirmation. This is not a read-only surface,
and this document does not claim it is:

- **Identity/artwork providers** (RomM, and platform artwork sources) fetch
  metadata and images over the network when a source is configured and a
  fetch is explicitly triggered (scan, refresh, or an explicit lookup). See
  `crates/archivefs-cli/src/romm_identity.rs` and the `romm_*` GUI modules
  under `crates/archivefs-gui/src/`.
- **Cheat/patch catalogue providers** (RetroArch catalogue, GameHacking.org
  for PS2/GameCube/Wii, BSFree Archive) perform certificate-validated HTTPS
  retrieval, bounded parsing, and provenance/hash recording of the fetched
  catalogue data. Catalogue retrieval alone does not install anything or
  modify an emulator's configuration - see `docs/CHEATS_MODS_SAFETY.md` for
  the fuller trust/inspection model these adapters follow.
- **Emulator-profile writes** (RetroArch, PCSX2, Dolphin/GameCube, Dolphin/Wii,
  Xenia cheat/patch installers) discover an emulator's profile directory
  read-only, and only ever write to it as an explicit install action the user
  previews and confirms - never automatically during discovery, a scan, or a
  catalogue fetch. Installers use atomic replacement and backups where
  applicable, verify the live target before writing, and record what they
  changed so the same install can be identified and reversed (managed-only
  removal / Undo). See `crates/archivefs-core/src/patch_manager/` and
  `docs/PATCH_CHEAT_MANAGER_DESIGN.md`.
- **Installer ownership of files on disk** (`install.sh`) is tracked and
  reasoned about separately from the above - see
  [`README.md`'s installer section](../README.md#quick-install) and the
  installer's own SHA-256 ownership-manifest logic. This document does not
  restate that mechanism.

No provider network access or emulator-profile write happens automatically on
startup, on a background timer, or as a side effect of mounting or
cataloguing archives.

## Path Safety

EmuWiz must not allow archive names or internal archive paths to escape the configured mount area.

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
- `PermissionDenied`: EmuWiz cannot read or mount the archive.
- `RetryAvailable`: a failed archive can be retried.

Retry behavior should be explicit. EmuWiz should not silently retry in a tight loop or hide repeated failures.

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

The current security posture is conservative, with a small number of
explicitly gated exceptions rather than a blanket read-only guarantee:

- Archives are mounted read-only through ratarmount.
- Mounting, inspection, and cataloguing never modify a source archive, ROM,
  or disc image.
- DAT-driven rename is the one path that does modify source files, and only
  after explicit per-item approval, generation and classifier-version gates,
  batch-and-per-entry preflight, a durable pre-mutation journal, no-clobber
  renames, and filesystem confirmation (see the DAT rename subsection above).
- Mount directories are generated under `mount_root`.
- Unmounting is restricted to paths under `mount_root`.
- The persistent catalogue, managed library views, and the read-only
  PCSX2 patch-preview feature all follow the same rule: they read or
  organize existing state, and none of them is a dependency of mount or
  unmount safety (see [ADR 0001](adr/0001-persistent-library-database.md)).
- Identity/artwork providers and cheat/patch catalogues reach the network
  only when explicitly triggered; emulator-profile writes require explicit
  preview and confirmation and are never automatic.
- Config and data directories prefer EmuWiz's own XDG paths and transparently
  fall back to the legacy ArchiveFS paths when only they exist; resolution is
  read-only and never copies, moves, or overwrites data.
- Native FUSE and Docker packaging remain out of scope. A desktop GUI now
  exists and reuses the same core safety checks as the CLI rather than its
  own permission model.
