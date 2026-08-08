# DAT Rename Apply — Stage 1 (gated, reversible)

Status: implemented on `feature/dat-rename-apply`. This is the design and
safety record for the **apply** side of the read-only rename planning (PR #14).
It may perform filesystem renames **only** for proposals that are from the
current validated plan generation, explicitly approved by the user, still
unchanged and safe at apply time, and that pass every preflight check
immediately before execution.

## 1. Threat model

Applying renames turns derived, catalogue-controlled data into filesystem
mutations. The threats this stage guards against:

| Threat | Mitigation |
| --- | --- |
| A hostile file swapped in after review | Identity snapshot at review time (size + kind + inode/device where supported); preflight re-checks it immediately before every rename, and the executor confirms the destination against the recorded identity after the rename. |
| Source replaced by a symlink / broken symlink | `symlink_metadata`-based identity; a symlink kind never matches a recorded regular-file identity, and symlink sources are never applicable. |
| Destination appearing after approval | `renameat2(RENAME_NOREPLACE)`: the existence check and the rename are one atomic syscall, so a destination that appears between preflight and rename is refused, never overwritten. |
| Overwriting an existing file | No-replace primitive only; there is no copy+delete fallback and no replace semantics. |
| Escaping the source directory / trusted roots | Same-directory requirement is structural; the destination basename must be a single safe component; canonical source and destination parents must lie inside the configured trusted roots. |
| Stale plan driving a rename | The plan generation is re-checked at build and at apply; a stale plan is rejected before anything runs. |
| Renaming the wrong object | The executor marks an entry Applied only after the filesystem confirms the source is gone and the destination matches the recorded identity. |
| A crash losing the record of what changed | A durable journal (temp file, `sync_all`, atomic rename, parent sync) is written before the first mutation and updated after every transition; recovery reads it and never auto-resumes. |
| Repeated/attended mutation | No retry of a failed mutation, no auto-apply, no background unattended renames, no "apply all" without explicit review + typed confirmation for large batches. |
| A partial rollback being presented as complete | Rollback verifies every step and reports fully / partially / failed. |

## 2. Preflight invariants

`run_preflight` is run for the whole batch, and again immediately before each
individual rename. Any failure means the entry is **not** renamed. Checks:

1. source still exists;
2. source is the same object as reviewed (identity: size, kind, and
   inode/device where supported; mtime used on platforms without inode
   identity);
3. source basename unchanged;
4. source is a regular file, never a symlink (a symlink loop or broken link is
   classified and refused, never followed);
5. destination does not exist;
6. no sibling whose name differs from the destination only by case appeared
   since the plan (re-checked against the live directory);
7. destination basename is still a single safe component (no `/`, `\`, NUL,
   `.`, `..`, empty);
8. destination parent is the same directory as the source parent;
9. source and destination parents canonicalise inside the configured trusted
   roots;
10. the plan generation matches the current one;
11. the proposal remains approved and actionable (Suggested, collision-free);
12. no two entries in the batch target the same destination.

## 3. Mutation primitive and no-clobber guarantee

`std::fs::rename` maps to `rename(2)`, which replaces an existing destination.
That is forbidden here. On Linux the executor uses
`renameat2(AT_FDCWD, src, AT_FDCWD, dst, RENAME_NOREPLACE)` via the already-
present `libc` dependency: a single atomic syscall that refuses if the
destination exists. There is deliberately **no** exists-then-rename sequence,
so there is no TOCTOU window. On platforms without a verified no-clobber
primitive the executor refuses to mutate.

Proven by tests: an existing destination is never overwritten; a destination
that appears during apply is refused atomically; a failed preflight changes
nothing; renames cannot escape the source directory; a symlink target is never
renamed.

## 4. Journal format

JSON at `~/.local/share/archivefs/rename-transactions/<transaction_id>.json`,
written with the durable atomic-write path (temp file, `sync_all`, rename,
parent-directory sync) **before** the first mutation and rewritten after every
transition. It records:

- `transaction_id`, `plan_generation`, `created_at_unix`, `source_scan_root`;
- overall `state` (Planned / Applying / Applied / ApplyFailed / RollingBack /
  RolledBack / RollbackFailed);
- per entry: source and destination paths, original and proposed basenames,
  the recorded identity snapshot, preflight result, `state`, failure reason,
  `applied_at_unix`, `rolled_back_at_unix`.

Unknown fields written by a future build round-trip verbatim (serde flatten),
so a newer journal never fails to be read. The journal contains full paths
because rollback requires them - this is the one place they are stored; general
History & Logs entries carry counts and the transaction id only. No secrets are
ever written.

## 5. Crash recovery

On entering the rename/apply screen the page lists journals whose state is not
settled and shows **"An interrupted rename transaction was found"** with three
options: **Review** (the transaction id, state, applied/total counts), **Roll
back completed steps**, and **Leave untouched**. Nothing auto-resumes; there is
no resume path at all. A journal that fails to parse is reported, never
deleted.

## 6. Rollback guarantees and limits

Rollback reverses only entries the filesystem confirmed as Applied, in reverse
order. For each entry it re-verifies the destination is still the recorded
object and the original source path is free, renames back with the no-clobber
primitive, and confirms the source is restored. Results are reported as fully
rolled back, partially rolled back, or rollback failed - a partial rollback is
never presented as complete. Repeated rollback is idempotent (entries already
rolled back are skipped; a fully rolled back transaction is a safe no-op).
Rollback limits: it cannot recover a destination that was changed or removed
externally, or an original name that was occupied - in those cases it stops and
reports, without overwriting anything.

## 7. Symlink behaviour

Planning and apply only ever use `symlink_metadata`; a link is never followed.
A source that is (or becomes) a symlink is refused. A rename never touches a
symlink target.

## 8. Batch behaviour

Approved proposals are preflighted as a whole. In `AbortAll` mode any hard
conflict stops the batch before it starts (nothing mutated, no journal). The
user may explicitly choose `SkipUnsafeSubset`, which journals the batch with
the conflicting entries marked Skipped and applies only the safe ones. Entries
are applied one at a time; the journal is persisted after every transition; a
failure stops the batch and offers rollback of what was already applied. There
is no implicit best-effort mode.

## 9. GUI confirmation boundary

- Only Suggested, actionable, approved proposals can enter Apply.
- Beginning the review builds the transaction at review time and shows the
  exact old → new names, the count, the trusted root, and a read-only preview.
- Batches larger than 8 require typing the exact phrase `RENAME N FILES`.
- Buttons: **Apply approved renames**, **Cancel**, **Roll back transaction**
  (and, after an AbortAll conflict, the explicit **Apply only the independently
  safe subset**).
- Apply is not offered for stale plans, ambiguous/conflicted/blocked proposals,
  symlink sources, or incomplete preflight.
- The GUI never calls `std::fs::rename`; the core executor owns every mutation
  and runs on a worker thread.

## 10. Unsupported cases

Symlink sources, broken symlinks, directories, archive-member renames,
multi-file sidecar coordination, cross-directory or cross-filesystem moves,
overwrites/replace semantics, background unattended renaming, and any rename
outside the configured trusted roots. Non-Linux platforms without a verified
no-clobber primitive refuse to mutate.

## 11. History & Logs

Apply and rollback outcomes record the transaction id, start/end time, counts
requested/applied/skipped/failed, and rollback status - never full private
paths (the journal holds those).

## 12. Verification

- `cargo test --workspace` (transaction model, journal, preflight, executor,
  rollback, crash recovery; hostile filesystem changes; no-clobber proofs;
  content-integrity proofs; repeated stress runs for destination races,
  cancellation, rollback, and crash-recovery fixtures).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- `cargo fmt --all --check`, `git diff --check`, `cargo audit`,
  `./scripts/security-scan.sh`.
- Isolated empty `HOME`/`XDG` run.

---

*Created: 2026-08-07*
*Builds on: DAT_RENAME_PLANNING_STAGE1.md and the approved
DAT_CHEAT_POLICY_{AUDIT,MODEL,GUI,MIGRATION}.md design documents.*
