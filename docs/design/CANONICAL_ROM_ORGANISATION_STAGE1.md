# Canonical ROM organisation — Stage 1 design

Status: implemented in `crates/archivefs-core/src/dat/rom_organisation/`.

EmuWiz can organise identified games into a user-configured **master ROM
root** under canonical, RomM-compatible platform directories — only after an
explicit read-only plan and explicit approval. Nothing moves merely because a
root is configured.

## Master ROM root

- Configured in `config.toml` as `master_rom_root = "/mnt/games/roms"` (core:
  `Config.master_rom_root`, CLI: `archivefs-cli rom-organise set-master-root`).
- Optional; `None` means organisation is not offered. Setting it never mutates.
- Must be absolute and not a filesystem root; `..` is rejected. It is preserved
  by every other config writer.

## Canonical platform slug rules

- The destination folder name **always** comes from the canonical platform
  id's RomM slug mapping (`IdentityCache::romm_slug_for_platform`), never from
  a display label. `PSP` → `psp`, `Xbox360` → `xbox360`, `Nintendo DS` → `nds`,
  `Switch` → `switch`, etc. — whatever the imported RomM cache defines.
- A missing slug mapping is `Unsupported` (the user must import a RomM
  identity cache), never a guessed folder name.
- The destination filename reuses the rename planner's canonical derivation
  (`derive_proposed_basename`); there is no second filename engine. Extension
  mismatches are `Unsupported`, traversal names are `Blocked`.

## Organisation modes

Three explicit, never-combined modes:

1. **Rename in place** — canonical name in the source's own directory.
2. **Move real file** — move the regular file into `master_root/<slug>/`.
3. **Organise symlink only** — move the symlink *object* into
   `master_root/<slug>/`, preserving the target text exactly and never
   dereferencing or touching the target. A regular file in this mode is
   `Blocked`; a symlink in move-real-file mode is `Blocked`.

Canonical symlink *creation* (leaving the real file and linking it) is out of
scope for Stage 1 and is not bluffed: it is not offered.

## Trusted-root rules

- The source's directory and the destination directory must both be inside the
  configured trusted roots. The caller (GUI/CLI) builds trusted roots from the
  source folders plus the master ROM root.
- Only canonical platform directories derived from trusted slugs are created
  during apply; a pre-existing directory is never recorded as ours.

## Planning

`build_organisation_plan` is read-only. For every candidate it resolves the
platform identity (`resolve_platform_identity`), derives the slug + filename,
computes the destination, and classifies:

- **Suggested** — safe to apply after approval.
- **Already organised** — already at the canonical destination.
- **Conflict** — destination occupied, case-only collision, or two plans to
  one destination.
- **Blocked** — unknown/conflicted platform, unsafe name, wrong object kind
  for the mode.
- **Unsupported** — missing slug mapping, extension mismatch, directory source.

Planning creates no directories and changes nothing; tests snapshot the tree
and assert zero changes.

## Collision handling

Detected in the plan and re-checked immediately before apply: exact
destination exists, case-only sibling, two plans to one destination, two plans
differing only by case, traversal/escape. Never auto-resolved.

## Same-filesystem restriction

- Moves use `renameat2(RENAME_NOREPLACE)` only (the shared no-clobber layer).
- The source and destination must be on the same filesystem (compared by
  device id). A cross-filesystem destination is refused with
  "cross-filesystem organisation is not yet supported safely".
- There is **no copy+delete fallback**. A future explicit transactional copy
  workflow is a later PR boundary.

## Symlink behaviour

- Symlink-only mode moves the link object; the target text is preserved and
  verified after the move. The target is never dereferenced, read, moved or
  deleted. Broken symlink objects may be moved (the object itself is the
  recorded identity).
- Symlink-to-directory objects may be moved as link objects (the target
  directory is never touched).
- Directory sources and archive members are unsupported.

## Journal / recovery

Reuses the shared `rename_apply` engine:

- durable journal of intent before any mutation;
- per-entry `Applying` checkpoint before each no-clobber move;
- platform-directory creation recorded in the transaction
  (`created_directories`) and journaled before it happens;
- crash reconciliation (`reconcile_recovery`) classifies in-flight entries
  against the live filesystem for arbitrary cross-directory moves;
- no auto-resume; an interrupted transaction is surfaced and rollback offered;
- cancellation before the first mutation moves nothing; cancellation
  mid-batch is reported honestly (never as fully rolled back).

## Rollback

- Reuses `rollback_transaction` to reverse every move with the same no-clobber
  guarantees (destination identity verified, source freed, `RENAME_NOREPLACE`).
- Then removes only the platform directories this transaction created
  (`created_directories`), in reverse order, only when they are empty and sit
  exactly one safe component beneath the master ROM root. A pre-existing user
  directory is never removed.

## Platform identity prerequisites

Organisation requires a platform identity resolved strongly enough: manual,
verified DAT, canonical RomM, or an accepted strong identity. Unknown or
conflicting identities are `Blocked`; if the identity changes after planning,
the plan's generation is stale and apply is rejected until the plan is
regenerated.

## Future copy-workflow boundary

Cross-filesystem organisation, canonical symlink creation, directory sources
and archive members are deliberately outside Stage 1. They require a
transactional copy/link engine with the same crash and rollback guarantees and
are future PRs.
