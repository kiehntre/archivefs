# RetroArch Cheat Installation Result and Journal Data Model

Successful writing runs are rollbackable through the journal using
[`retroarch-cheat-rollback`](RETROARCH_CHEAT_ROLLBACK.md). The rollback parser
consumes this journal's recorded outcomes and hashes; it does not trust stored
absolute paths without revalidating them against the caller's destination root.

This document covers `patch_manager::cheat_install_result` - stable,
serializable Rust types and pure conversion logic describing what would
happen, and now (via `archivefs retroarch-cheat-install`; see
[`RETROARCH_CHEAT_INSTALL.md`](RETROARCH_CHEAT_INSTALL.md)) what actually
did happen, when a cheat from the
[external cheat catalogue](RETROARCH_CHEAT_CATALOGUE.md) is installed.

**This module itself still contains no installer.** Nothing in
`cheat_install_result` reads a cheat file's bytes, opens or creates a
destination path, writes a file, creates a backup, or writes a journal to
disk - it defines the data model only, the *shape* of a result and a
journal entry. The separate `cheat_installer` module (documented in
[`RETROARCH_CHEAT_INSTALL.md`](RETROARCH_CHEAT_INSTALL.md)) is the real
execution engine that populates these types for a genuine run, without
redefining any of them. See
[`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md)'s Phase 3
for the full transactional install/journal/rollback design this remains a
narrower slice of - content-addressed backup storage, manifest
generations, and a full crash-recovery state machine are still not
implemented.

## Relationship to the destination-path/symlink-safety module

The `destination_safety` module provides the reusable destination-path
resolution and symlink-safety primitives both the staging preview and the
real installer use to decide *where* a cheat goes and whether a
destination is safe to touch. This module does not duplicate, reimplement,
or depend on the internals of that work: it only *describes results* in
terms of values a resolver (the staging preview, or the real installer's
own fresh revalidation immediately before writing) already computed and
handed it. Nothing here resolves a path, follows or rejects a symlink, or
decides whether a hint is safe - seeing a `destination_path` field here is
a description of a decision made elsewhere, never a decision made by this
module.

## Purpose

Answer, for every catalogue entry a staging preview already evaluated:

- If installation were attempted, what would happen - a new file, a
  no-op (already installed), a content replacement, or a refusal?
- Why - a stable, machine-readable reason, not just prose?
- What would it cost - would a filesystem write be required at all?
- Across a whole run, how many entries fall into each bucket, and what is
  the run's overall disposition?

And, for a future execution phase, provide the same shape to record what
*actually* happened - a durable-shaped journal entry - without this
milestone committing to any particular durability mechanism.

## Per-entry result: `CheatInstallEntryResult`

One result per catalogue entry. Every field is either copied from an
already-computed [`CheatStagingPlan`](RETROARCH_CHEAT_CATALOGUE.md#staging-preview)
(never invented) or set to `None`/`false` when this milestone has no way to
know it yet (see the fields marked "always `None`/`false` from
`plan_cheat_install_entry`" in the source doc comments):

| Field | Meaning |
| --- | --- |
| `source_path` | The catalogue source file. |
| `expected_source_hash` | Hash recorded when the catalogue was parsed. |
| `observed_source_hash` | Hash from a future executor's revalidation - always `null` today. |
| `destination_path` | Proposed destination, when one could be resolved. |
| `previous_destination_state` | `absent` / `present_matching_source` / `present_different` / `unknown` - derived from the staging plan's own action, never freshly probed. |
| `previous_destination_hash` | The existing destination's hash, when known. |
| `backup_path` | Where a pre-replacement backup was/would be written - always `null` today; nothing in this codebase creates backups yet. |
| `resulting_destination_hash` | The destination's hash after a real write - always `null` today. |
| `outcome` | See "Outcome codes" below. |
| `reason_code` | A stable identifier for why `outcome` was chosen. |
| `detail` | Optional human-readable detail lines - never the primary machine-readable state. |
| `applied` | `true` only when a real write actually happened - always `false` today. |
| `eligible` | Whether staging's match/eligibility rules allowed this entry to become an actionable candidate at all, independent of run-level policy. |
| `write_required` | Whether carrying out `outcome` would need a filesystem write. |

Paths use `CheatInstallPath` (`{ display, lossy }`) - the same lossless-safe
shape as the rest of this codebase's `EncodedPath`, defined locally in this
module so it can also implement `Deserialize` (see the module's own doc
comment for why). A path is never round-tripped through a bare, possibly
information-losing `String`/`PathBuf` conversion.

## Outcome codes

Stable, lower-snake-case, and serialized as the primary machine-readable
state - `reason_code`/`detail` explain an outcome, they never replace it:

- `installed_new`, `already_installed`, `replaced_with_backup`,
  `skipped_replace_not_allowed`, `skipped_not_eligible`,
  `skipped_conflict` - the only six values [`plan_cheat_install_entries`]
  can produce today, from an already-built staging preview.
- `skipped_source_changed`, `skipped_destination_changed`,
  `failed_unsafe_path`, `failed_backup`, `failed_write`,
  `failed_verification` - reserved for a future executor that actually
  revalidates and writes. Nothing in this codebase can produce these yet.

The same outcome value is used whether a result is a preview or (in the
future) a real completed action - see "Dry-run semantics" below for how
those are told apart without renaming the outcome.

## Pure bridge from the staging preview

[`plan_cheat_install_entry`]/[`plan_cheat_install_entries`] map an existing
`CheatAvailabilityEntry` (from `retroarch-cheat-catalogue`'s staging
preview) to a planned result, with one run-level policy switch,
`allow_replace_different`:

| Staging `planned_action` | Result `outcome` |
| --- | --- |
| `install_new` | `installed_new` (eligible, `applied: false`) |
| `already_installed` | `already_installed` (no write required) |
| `replace_different`, `allow_replace_different: true` | `replaced_with_backup` (eligible, `applied: false`) |
| `replace_different`, `allow_replace_different: false` | `skipped_replace_not_allowed` (still `eligible: true` - the match was fine; only run policy declined it) |
| `conflict` | `skipped_conflict` |
| `not_eligible` (weak/ambiguous/unsupported match, incomplete parsing, unresolved platform, unsafe path, no environment) | `skipped_not_eligible` |

This mapping performs no filesystem access, no destination probing, and no
path validation of its own - every value it reports was already decided by
the staging preview's existing resolver.

## Run-level result: `CheatInstallRun`

```text
schema_version
run_id
started_at_unix_seconds
completed_at_unix_seconds
dry_run
allow_replace_different
destination_root
catalogue_source
entries
summary
status
```

`run_id` and every timestamp are always caller-supplied - nothing in this
module reads the system clock, so pure tests never need to mock one.
[`plan_cheat_install_run`] is the only constructor this milestone offers,
and it always sets `dry_run: true` (see below): there is no executor yet to
produce a non-dry-run one.

## Derived summary: `CheatInstallSummary`

Every field - `requested`, `eligible`, `installed_new`,
`already_installed`, `replaced`, `skipped`, `failed`, `backups_created`,
`writes_required`, `writes_attempted`, `writes_succeeded`,
`dry_run_actions` - is computed by [`CheatInstallSummary::from_entries`]
from the entry list itself. There is no other way to construct one, and no
field is ever incremented independently while entries are built, so a
summary can never drift from the results it describes.

## Dry-run semantics

A dry run reports what *would* happen without claiming anything happened:

- The outcome is the same value a real execution would use
  (`installed_new`, `replaced_with_backup`, ...) - dry-run status is never
  encoded by picking a different outcome.
- `applied: false` on every entry.
- `dry_run: true` at the run level.
- An entry that would require a write is counted in `writes_required` and
  `dry_run_actions`, **never** in `writes_attempted`/`writes_succeeded` -
  those two fields are always `0` for a dry run.

## Run status: `CheatInstallRunStatus`

`success` / `partial_failure` / `failed` / `dry_run`, derived by
[`CheatInstallRunStatus::derive`]:

1. `dry_run` always wins first - a dry run is never `success`/`failed` in
   the execution sense, because nothing executed.
2. Otherwise, zero `failed` entries is `success`.
3. At least one `failed` entry alongside at least one
   `installed_new`/`already_installed`/`replaced` entry is
   `partial_failure`.
4. At least one `failed` entry with none of those is `failed`.

`skipped_*` outcomes (including `skipped_not_eligible`) never affect this
status on their own - declining an ineligible, conflicting, or
policy-disallowed entry is not an execution failure.

## Schema versioning

`schema_version` (currently `1`, `CHEAT_INSTALL_RUN_SCHEMA_VERSION`) is a
plain field, but reading one back should go through
[`parse_cheat_install_run`] rather than a bare `serde_json::from_str`: it
explicitly rejects an unrecognized `schema_version` instead of letting a
future or unknown schema silently populate today's field set with the
wrong meaning. Unknown *additional* JSON fields are still accepted and
ignored on read (no `deny_unknown_fields`), so a future minor addition to
this schema does not by itself break reading an older or newer document
back with the same major schema version.

## Non-goals (of this data-model module itself)

This module - `cheat_install_result` - still does not, and never will:

- copy, install, replace, or delete any cheat file itself;
- create a destination directory or the destination file itself;
- create a backup;
- write a journal (or anything else) to disk;
- perform destination probing, path resolution, or symlink-safety
  validation - that remains entirely inside
  [`destination_safety`](RETROARCH_CHEAT_CATALOGUE.md); or
- read the system clock, an environment variable, or any other live
  process state (the executor that now consumes this module, described
  below, does read the clock for its own real timestamps - this module's
  own pure types and functions still never do).

**A real, write-capable executor now exists and consumes this exact data
model without redefining it**: see
[`RETROARCH_CHEAT_INSTALL.md`](RETROARCH_CHEAT_INSTALL.md) for
`archivefs retroarch-cheat-install` - the command that actually creates
files, backups, and journals using [`CheatInstallRun`]/
[`CheatInstallEntryResult`]/[`CheatInstallSummary`]/
[`CheatInstallRunStatus`] as defined here, and
[`plan_cheat_install_entry`]/[`plan_cheat_install_entries`] as its starting
point for every result it produces. This document's description of the
*shape* of a result, a run, and a journal entry remains accurate; only the
claim that nothing yet populates them for real is superseded.
