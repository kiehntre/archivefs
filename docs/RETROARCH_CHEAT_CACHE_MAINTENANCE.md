# RetroArch cheat snapshot maintenance

ArchiveFS publishes a retrieved cheat catalogue as a content-addressed
snapshot. Snapshot contents and their per-snapshot manifest are immutable;
maintenance never edits or repairs either one. This document covers the
deliberate inventory, verification, pinning, and pruning operations around
those snapshots.

All commands are offline. They do not contact a source, install cheats, read a
RetroArch configuration or library database, or enable cheats.

## Commands

```text
archivefs retroarch-cheat-snapshot-list [--source <source-id>] [--cache-root <path>] [--json]
archivefs retroarch-cheat-snapshot-verify <snapshot-id> [--cache-root <path>] [--json]
archivefs retroarch-cheat-snapshot-verify --source <source-id> [--cache-root <path>] [--json]
archivefs retroarch-cheat-snapshot-verify --all [--cache-root <path>] [--json]
archivefs retroarch-cheat-snapshot-pin <snapshot-id> [--cache-root <path>] [--json]
archivefs retroarch-cheat-snapshot-unpin <snapshot-id> [--cache-root <path>] [--json]
archivefs retroarch-cheat-cache-prune [policy] [--cache-root <path>] [--json]
```

The default root remains
`~/.local/share/archivefs/cheat-sources/`. `--cache-root` is explicit and uses
the same traversal and no-symlink validation as retrieval. A missing root is
an empty read-only inventory; listing, verification and prune preview do not
create it.

## Inventory and verification

Inventory enumerates snapshot directories deterministically by source,
newest recorded retrieval timestamp, identity and lossless path. A directory
name is never sufficient evidence. ArchiveFS reads the corresponding manifest,
checks its schema, source/hash/cache-path binding, and reuses retrieval's
bounded tree walk and SHA-256 file manifest. It reports provenance, archive and
expanded sizes, entry count, freshness, current/last-known-good status, pins,
and integrity findings.

Verification can address one unambiguous snapshot digest prefix (at least
eight hexadecimal characters), all snapshots for a source, or the whole
cache. It distinguishes invalid manifests, identity mismatches, missing or
unexpected files, size and digest mismatches, unsafe paths, unsupported
schemas, unreadable paths, and unpublished staging artifacts. It is strictly
read-only and returns a non-zero command status when an inspected entry is
invalid. It never repairs, rewrites, migrates or deletes an invalid snapshot.

ArchiveFS does not infer a last-use time from filesystem access times because
that would be unreliable and platform-dependent. `last_successful_use` is
therefore currently absent. The retrieval schema has one validated `current`
pointer; that same snapshot is conservatively treated as last-known-good.

## Pins

Pins are stored atomically in `<cache-root>/<source-id>/pins.json`, outside
both the snapshot and immutable manifest. Pin and unpin are idempotent and
survive later fetches. A snapshot ID must resolve uniquely to a valid,
manifest-bound snapshot. Malformed, wrongly bound or symlinked pin metadata is
rejected; pruning then protects every affected snapshot because pin state is
unknown.

A pin only prevents ArchiveFS pruning. It cannot prevent the filesystem owner
from manually changing or deleting cache data. Verification detects such
changes.

## Prune planning

Pruning always builds a complete plan first. With no policy the plan deletes
nothing. Supported policy inputs are:

- `--keep <count>`: retain the newest count per source;
- `--older-than-days <days>`: consider only snapshots older than the supplied
  age;
- `--max-cache-bytes <bytes>`: select the oldest otherwise-unprotected
  snapshots needed to reach the budget where possible. The accounting covers
  logical snapshot-file and matching-manifest bytes; protected source/pin
  metadata and filesystem allocation overhead are not reclaimable candidates;
- `--source <source-id>`: restrict the plan;
- `--include-abandoned-staging`: include staging entries older than the
  conservative minimum;
- `--abandoned-staging-min-hours <hours>`: explicitly override the default
  24-hour minimum, but never below the enforced one-hour safety floor.

Combined count and age rules are conservative: a snapshot protected by either
retention rule stays protected. Size budgeting never overrides a pin, current
or last-known-good pointer, required keep count, age retention, failed
verification, malformed metadata, or an unsafe/ambiguous path.

Every entry carries deterministic reasons such as `exceeds_keep_count`,
`older_than_retention`, `exceeds_cache_budget`, `pinned`, `current`,
`last_known_good`, `within_retention`, `required_keep_count`,
`verification_required` or `unsafe_or_ambiguous_path`.

`retroarch-cheat-cache-prune`, `--dry-run`, JSON mode and non-terminal use are
preview-only unless `--yes` is explicitly supplied. ArchiveFS never prompts
from this command and has no automatic or background pruning.

## Confirmed deletion and staging cleanup

For every candidate a confirmed run immediately repeats inventory, manifest
and file-digest verification; reloads current and pin metadata; compares the
planned manifest token and byte count; reconstructs the exact path beneath the
selected cache root; and rejects symlinks or changed identities. Independent
candidates continue after a changed, unsafe or failed entry. Results distinguish
`deleted`, `skipped`, `changed`, `unsafe` and `failed` and report logical file bytes
actually removed.

Snapshot deletion removes only the exact content-addressed directory and its
matching immutable manifest. It never removes the cache root, source metadata,
pin metadata, retained snapshots, or catalogue files outside the selected
ArchiveFS cache. Empty parent directories are deliberately left in place.

Retrieval's `.staging` entries are not published snapshots. Cleanup considers
only exact children of a source's `.staging` directory. Recent entries are
treated as possibly active. Missing timestamps, non-UTF-8 source identities,
symlinks, special files and ambiguous paths remain protected. Cleanup is
preview-only until the same explicit `--yes` confirmation.

## JSON, failures and history

Every command supports stable schema-versioned JSON with lower-snake-case
states and encoded paths. JSON never prompts or mixes prose into stdout.
Malformed entries remain visible as structured findings instead of aborting a
whole inventory; an unsafe cache root or malformed pin operation is a clear
failure.

Applied maintenance history is not written in this milestone. Existing cheat
history is intentionally bound to installation and rollback journal schemas,
not generic cache operations, and ArchiveFS has no standalone operation-history
store that could be reused without adding an unrelated database dependency.
The confirmed result itself is the complete auditable record and may be saved
by callers. A future shared operation-journal design can persist it without
changing snapshot or pin formats.

## Examples

```text
archivefs retroarch-cheat-snapshot-list
archivefs retroarch-cheat-snapshot-verify --all --json
archivefs retroarch-cheat-snapshot-pin c02d6ea1
archivefs retroarch-cheat-cache-prune --keep 3 --older-than-days 90
archivefs retroarch-cheat-cache-prune --keep 3 --older-than-days 90 --yes
archivefs retroarch-cheat-cache-prune --include-abandoned-staging --dry-run
```

Current limitations are the absence of reliable last-use timestamps, a single
current/last-known-good pointer in retrieval metadata, no automatic cache
budget enforcement, no automatic pruning, and no persisted maintenance-run
journal. Safety revalidation narrows concurrent-change races but does not lock
out manual filesystem changes by the cache owner; any detected change blocks
that candidate.
