# DAT Rename Planning — Stage 1 (read-only)

Status: implemented on `feature/dat-rename-planning`. This document is the
design and safety record for the **read-only rename planning** layer built on
the DAT audit and the PR #13 matching policy.

## 1. Purpose and hard rule

ArchiveFS can verify a local ROM against a DAT catalogue and, with the
user's matching policy, say *which* catalogue entry a file is. This stage turns
that into a **proposal**: a suggested canonical filename, why it was chosen,
and anything that blocks it.

**Hard safety rule: actual rename execution is NOT implemented.** Nothing in
this PR renames, moves, deletes, rewrites, chmods, truncates, replaces, or
otherwise mutates a ROM or game file. Planning only:

- inspects an existing audit result and read-only `symlink_metadata`;
- derives a proposed canonical filename from the authoritative DAT entry;
- explains why and surfaces ambiguity and conflicts;
- records session-only review decisions about proposals.

## 2. Threat model

A rename planner is a new way for *derived, catalogue-controlled data* to
influence what a file is called. The threats this stage guards against:

| Threat | Mitigation |
| --- | --- |
| A malicious/corrupt DAT entry name escapes the target directory | The proposed name must be a single path component; `/`, `\`, NUL, `.`, `..` and empty names are **Blocked**, never sanitised into a traversal. |
| Silent format change | A proposed name whose extension differs from the source file's is **Unsupported**: renaming `game.zip` to `game.iso` would silently change what the file claims to be. |
| Filesystem-invalid names on the target OS | Invalid characters are replaced with `_` and the replacement is explained (deterministic per platform). |
| Weak evidence driving a rename | Only cryptographic-hash exact matches produce proposals; CRC32 / filename-only evidence is never promoted. |
| Silent ambiguity resolution | When the policy cannot pick a winner, the proposal is `Ambiguous` and no name is proposed. Deterministic display order is never a decision. |
| Collisions destroying a file | Existing targets, case-only collisions, and two-proposals-one-target are all `Conflict`; nothing is resolved automatically. |
| Symlink dereference / rewriting | A symlinked source is `Unsupported` (a future apply stage would rename the link itself, never its target); planning never follows or rewrites a link. |
| Stale data masquerading as current | Plans carry a generation; a plan from a stale audit generation is rejected. |
| Planning becoming a write | The plan builder's only filesystem access is `symlink_metadata` per verified source; sibling names come from the audit's own file list. A snapshot test proves paths, inodes, sizes and contents are unchanged. |

## 3. Read-only guarantees

- No `create`, `write`, `rename`, `remove`, `chmod`, `truncate`, `symlink` or
  `readlink`-driven mutation anywhere in the plan module.
- The only filesystem calls are `std::fs::symlink_metadata` (per verified
  source, to classify it) and the audit's already-recorded data.
- The GUI builds the plan on the audit worker thread, never hashing or
  re-scanning; it is cancellable.
- Enforced by `planning_makes_no_filesystem_mutation` (core) and the GUI tests
  that snapshot a directory before and after review-decision edits.

## 4. Proposal states

`ProposalState` (no variant implies a rename happened):

| State | Meaning | Actionable |
| --- | --- | --- |
| `Suggested` | A verified match produced a canonical name different from the current one, with no collision. | yes (future stage) |
| `AlreadyCanonical` | The current name already equals the proposed name. | no |
| `Ambiguous` | The policy could not pick a winner among verified candidates. | no |
| `Conflict` | A collision blocks the proposal (existing target, case-only, or two-proposals-one-target). | no |
| `Unsupported` | No safe canonical name exists (extension/container mismatch, or a symlink source). | no |
| `Blocked` | No canonical name could be derived (path traversal, empty name, missing source). | no |

Every proposal carries: source path identity, current and proposed basenames,
platform, DAT source, matched game/ROM names, match verdict and confidence,
policy explanations, ambiguity reason, collision detail, blockers, extension
status, sanitisation notes, object kind, and `actionable`.

## 5. Canonical filename derivation

`derive_proposed_basename(rom_name, source_basename)` is a pure function:

1. reject empty, `.`/`..` (Blocked);
2. reject any `/`, `\` or NUL (Blocked - never sanitised into a traversal);
3. compare extensions case-insensitively: equal or both-absent ⇒
   `Preserved`; different ⇒ `Unsupported` (container/member rename not
   supported; no such mapping exists in ArchiveFS today);
4. replace filesystem-invalid characters (control characters everywhere;
   the Windows-reserved set on Windows) with `_`, recording each replacement;
5. re-check the result is a non-empty single component.

The proposed name is **only** the matched DAT entry's ROM name (`rom_name`).
Nothing is invented.

## 6. Policy integration

For a single `Exact` verdict the proposal uses the verdict's game/ROM names
with no ranking (nothing was ranked). For `ExactMultipleCandidates` the PR #13
`CandidateResolution` supplies the winner and its `explanations`
("preferred region matched (Europe)", "newer verified revision preferred
(Rev 2)", "source priority 20 outranked source priority 100", "parent
preferred"). If the resolution is not decided, the proposal is `Ambiguous`.

Rules: no weak-evidence candidate may outrank a verified one (only
`Exact`/`ExactMultipleCandidates` produce proposals); deterministic display
ordering never masquerades as a decision.

## 7. Collision handling

Detected read-only, never resolved:

- a sibling with the exact proposed name exists ⇒ `Conflict`
  (target already exists);
- a sibling differing only by case ⇒ `Conflict` (case collision);
- two proposals in one directory sharing a target ⇒ both `Conflict`
  (nothing "wins");
- the colliding path being a symlink is surfaced in the detail.

The sibling index is built from the audit's own file list, so no second scan
is required.

## 8. Symlink handling

- A source that is a symlink ⇒ `Unsupported`: a future apply stage would
  rename the link itself, never its target, and that is not promised.
- A broken symlink ⇒ `Unsupported`: planning cannot verify what a rename
  would move.
- Planning never dereferences or rewrites a link; `symlink_metadata` only.

## 9. Review decisions (session-only)

`ReviewDecision` = `AcceptedForReview | Ignored | NeedsManualReview`. They are
decisions **about the proposal only** and never trigger file operations.

**Deferral:** decisions are kept in the GUI session only. `dat_sources.toml`
owns preferences, and per-file review state belongs in the library database,
which a schema migration would be needed to extend - that is the one-way door
this stage does not cross. Persisting review decisions is a future,
separately-approved step.

## 10. Plan generation

`build_rename_plan(outcome, context, cancel)`:

- reads only the audit outcome plus `symlink_metadata` per verified source;
- builds the sibling index from the audit's file list;
- is cancellable between files and never runs on the GUI thread;
- carries a `generation`; `plan_matches_generation` lets a caller reject a
  stale plan;
- a truncated audit produces a clearly-labelled partial plan.

## 11. GUI surface

A "Rename planning" section on the DAT Sources page, shown after an audit:

- prominent **"Planning only — ArchiveFS will not rename any files"** banner;
- counts by state and filters (All / Suggested / Already canonical / Ambiguous
  / Conflicts / Unsupported / Blocked);
- per-proposal rows with current → proposed names, platform, source, match
  verdict, policy explanations, ambiguity/conflict/blocker detail;
- actions limited to review decisions, resetting them, and copying a proposed
  name. No Apply/Rename/Execute/Commit/Move/Delete control exists.

## 12. Deferred apply/rollback boundary

Applying a rename is **not** part of this stage and not designed here. A future
PR #15 would define, separately approved: a rename apply plan over the
`actionable` (`Suggested`) proposals, collision re-checks at apply time,
backup/rollback semantics, and the symlink rules for an actual rename. Nothing
in this stage implements, stubs, or advertises that work.

## 13. Verification

- `cargo test --workspace` (core plan tests cover states, derivation,
  collisions, symlinks, determinism, stale generation, and a no-mutation
  snapshot; GUI tests cover filters, compact width, keyboard navigation, the
  planning-only warning, the absence of apply controls, and that review
  decisions never touch files).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- `cargo fmt --all --check`, `git diff --check`, `cargo audit`,
  `./scripts/security-scan.sh`.
- Isolated empty `HOME`/`XDG` run.

---

*Created: 2026-08-07*
*Builds on: DAT_SOURCES_STAGE1_IMPLEMENTATION.md,
DAT_SOURCES_STAGE2_IMPLEMENTATION.md and the approved
DAT_CHEAT_POLICY_{AUDIT,MODEL,GUI,MIGRATION}.md design documents.*
