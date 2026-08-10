# DAT & Cheat-Source Policy Migration

Status: design proposal. This document is built on the approved baseline
[`docs/design/DAT_CHEAT_POLICY_AUDIT.md`](DAT_CHEAT_POLICY_AUDIT.md) and its
companions [`docs/design/DAT_CHEAT_POLICY_MODEL.md`](DAT_CHEAT_POLICY_MODEL.md)
and [`docs/design/DAT_CHEAT_POLICY_GUI.md`](DAT_CHEAT_POLICY_GUI.md). It
specifies how the current cheat-source configuration and on-disk state move into
the new policy model, how the new documents are versioned, and how users can
roll back safely. No Rust code is implemented here.

Grounding: current cheat-source configuration is the nine-entry
`cheat_source_registry` plus the per-user preferences file
`~/.config/archivefs/cheat_sources.toml`, alongside the retrieval pipeline's
`CheatSourceDefinition` and its cache/manifest state (audit §2, §3); current DAT
behaviour is CLI-only with no persistence (audit §1, §3). The migration must
therefore do most of its work for cheat sources today and leave DAT sources
ready to adopt the same layer when DAT persistence is approved.

> REVIEW FIX — this document originally assumed cheat-source configuration was
> compiled-in only, with no user-writable state. PR #2 shipped a user-writable
> TOML file. That changes the migration from "write a new document" to "grow an
> existing document that users may already have edited", and it introduces the
> `deny_unknown_fields` constraint in §3.2 that now gates the whole design.

---

## 1. Migration principles

1. **Behaviour equivalence is the acceptance test.** After migration, every
   operation that does not involve a newly added policy field must behave
   exactly as it does today. The mapping table (§2) and the behaviour-equivalence
   checklist (§6) are the executable definition of "no silent behaviour change".
2. **Policy is additive.** The new policy document starts as a thin envelope
   over existing state; nothing about parser, cache, lock, snapshot, or install
   behaviour changes unless a specific field is set.
3. **State is never thrown away.** Runtime state, cache, manifests, pins, and
   provenance survive migration and survive rollback.
4. **Migration is explicit and reversible.** The first migration writes a new
   policy document from current state; the old representation remains readable
   and is the rollback target.
5. **No automatic adoption of new behaviour.** New fields that have no current
   equivalent (priority, region/language preferences, clone policy, rename
   safety, etc.) are written at their safe defaults; they only change behaviour
   when the user changes them through the GUI.

---

## 2. How current cheat-source settings map into the new model

### 2.1 Source-level mapping

| Current setting / state | Source | New model field | Migration value | Behaviour effect |
| --- | --- | --- | --- | --- |
| `CheatSourceDefinition.source_id` | `trusted_retroarch_cheat_sources()` | `SourceId` | copied verbatim (`libretro-buildbot-cheats`) | Identity unchanged. |
| `CheatSourceDefinition.display_name` | registry | `display_name` | copied verbatim | Display unchanged. |
| `trust_status = "built_in_reviewed"` (computed in `list_retroarch_cheat_sources`) | registry | `SourceTrustLevel` | `BuiltInReviewed` | Trust label unchanged. |
| `CheatSourceDefinition.enabled` | registry | `enabled` | copied verbatim (`true`) | Participation unchanged. |
| `CheatSourceDefinition.experimental` | registry | (retained in source metadata) | copied verbatim | No policy meaning; kept for display. |
| `CheatSourceDefinition.download_url` / `permitted_host` / `revision_url` / `revision_host` / `canonical_repository_url` / `catalogue_prefix` / `archive_type` | registry | provenance + network/resource policy (audit §6.3) | copied verbatim into the source record | URL/host policy unchanged. |
| `CheatSourceDefinition.maximum_expected_bytes` | registry | resource policy (audit §6.1, §6.5) | copied verbatim (256 MiB) | Download ceiling unchanged. |
| `CheatSourceDefinition.expected_sha256` (registry-level, currently `None`) | registry | verification config | copied verbatim (empty) | No verification change. |
| `CheatSourceDefinition.pinned_version` | registry | revision policy (`Pinned` pin) | copied verbatim (currently `None`) | No change while empty. |
| `CheatSourceDefinition.provenance` / `licence_url` | registry | provenance | copied verbatim | Display/audit unchanged. |

### 2.1a Registry mapping (PR #2 types)

The retrieval definition above covers one source. The other eight, and the
user's preferences, map as follows:

| Current field | Source | New model field | Migration value | Behaviour effect |
| --- | --- | --- | --- | --- |
| `CheatSourceSpec.id` | `build_default_registry()` | `SourceId` | copied verbatim | Identity unchanged. |
| `CheatSourceSpec.default_priority` | registry | `priority` default | copied verbatim (10/20/30/40/50/60/65/70/100) | Order unchanged. |
| `CheatSourceSpec.platforms` | registry | platform participation | copied verbatim; empty = all platforms | Unchanged. |
| `CheatSourceSpec.capabilities` | registry | capability gate on policy fields | copied verbatim | Unchanged; determines which controls apply (GUI §4.5). |
| `CheatSourceSpec.emulator` / `upstream_project` / `description` | registry | provenance display | copied verbatim | Display unchanged. |
| `CheatSourceEntry.enabled` | registry + config | `enabled` | `true` unless the config says otherwise | Unchanged. |
| `CheatSourceEntry.health` | runtime | `SourceRuntimeState` | `None` preserved as "not checked" | Unchanged. |
| (no trust field) | — | `SourceTrustLevel` | **new**: `BuiltInReviewed` for all nine | No behaviour change; adds a label that did not exist. |
| `ProviderConfigEntry.enabled` / `.priority` | `cheat_sources.toml` | same fields | read as-is; **file not rewritten** | Unchanged. |
| `PlatformOverrideEntry.disabled_providers` | `cheat_sources.toml` | platform participation (model §6.2) | read as-is | Unchanged. |
| `PlatformOverrideEntry.priority_overrides` | `cheat_sources.toml` | per-platform priority | read as-is, clamped `1..=999` | Unchanged. |

### 2.2 Retrieval behaviour mapping

Today's retrieval behaviour is produced by `fetch_retroarch_cheat_source`
options plus constants (audit §2.2, §3.2). Each becomes an explicit policy or
runtime field:

| Current behaviour | New location | Migration value | Behaviour effect |
| --- | --- | --- | --- |
| Resolve `master` to exact commit, download immutable archive | revision policy | `FollowLatest` (model §8) | Identical: resolve-once, pin, reuse until refresh. |
| 24-hour freshness window | runtime state | `freshness` from manifest timestamp | Identical; freshness is derived, not policy. |
| Reuse fresh snapshot unless `--force-refresh` | runtime + fetch options | fetch options remain per-invocation | Identical; not persisted policy. |
| `--offline` reuses snapshot with no network | runtime + fetch options | fetch options remain per-invocation | Identical; not persisted policy. |
| `--expected-sha256` extra digest check | verification config | per-source `expected_sha256` (GUI §3.8) | Identical when unset; GUI can persist it later. |
| `--max-download-bytes` bound | resource policy | per-source download limit | Identical when unset (registry ceiling used). |
| Cancellation points | runtime | `CheatSourceCancellation` unchanged | Identical. |
| HTTPS-only, proxy-disabled, bounded transport, redirect/DNS policy | network policy (audit §6.3) | copied into shared net policy | Identical. |
| ZIP extraction safety | shared zip policy (audit §6.6) | copied into shared policy | Identical. |

### 2.3 Cache / manifest / persistence mapping

| Current on-disk state | New location | Migration value | Behaviour effect |
| --- | --- | --- | --- |
| `<cache-root>/<source-id>/metadata.json` (`CheatSourceCacheMetadata`) | runtime state | retained as-is; read through the model | Read path unchanged. |
| `<cache-root>/<source-id>/manifests/<sha256>.json` (`CheatSourceManifest`) | provenance/verification record | retained as-is | Immutable manifests unchanged. |
| `<cache-root>/<source-id>/snapshots/<sha256>/…` | runtime state / provenance | retained as-is | Content-addressed snapshots unchanged. |
| `pins.json` | revision policy (`Pinned`) | pins read into the policy layer; kept on disk | Pin behaviour unchanged. |
| lock file / `flock` on cache root | cache policy (audit §6.4) | shared cache-root policy | Lock semantics unchanged. |
| `.staging/` cleanup | runtime | unchanged | Identical. |

### 2.4 CLI-flag → policy mapping (no persistence unless exposed)

The following CLI flags are per-invocation today and remain per-invocation. The
model treats them as *ephemeral overrides* that never mutate the policy
document:

- `--force-refresh` → temporary `FollowLatest + refresh` hint (runtime).
- `--offline` → temporary "no network" hint (runtime).
- `--expected-sha256` → temporary verification override (validation only).
- `--max-download-bytes` → temporary resource-limit override (validation only).
- `--cache-root` → temporary cache-root override (path policy applied).

Rule: **an ephemeral override may lower a bound or tighten a requirement, never
raise a bound or loosen a requirement** relative to the policy document. This is
the same relationship the GUI's "differs from default" marker displays.

### 2.5 DAT source mapping

Today DATs have no registry, no persistence, and no GUI (audit §1.3, §1.4).
The migration for DATs is therefore a *new* registration step, not a data
migration:

- Existing CLI invocation behaviour (`dat inspect` / `validate` / `audit`) is
  unchanged.
- If/when DAT persistence is approved, previously CLI-typed DAT paths are not
  auto-registered; the user registers them explicitly through the GUI
  (GUI §1). Nothing is inferred from shell history or config.
- Defaults for a registered DAT source: `Untrusted`, disabled, priority `100`
  **in the DAT priority space** (model §5.2, §5.5). No band needs to be reserved
  relative to the 10–100 cheat priorities, because the two spaces are never
  compared. All matching-policy fields at safe defaults (model §4, §5, §7–§11);
  rename safety is fixed at `NeverSuggest` and is not a field the user sets
  (model §12).

---

## 3. Schema versioning

### 3.0 Prerequisite: lossless round-trip (must ship first)

**DECISION (approved): unknown provider IDs and unresolved platform overrides
must round-trip without data loss, and this is a prerequisite bug fix that ships
before any schema migration.**

Current behaviour loses data on write:

| Case | Today | Required |
| --- | --- | --- |
| `providers[]` entry with an ID not in the registry | `apply_config` skips it; `to_config()` re-serialises only live registry entries, so the next save **deletes the line** | Preserved verbatim; surfaced as "not recognised — kept as written" |
| `platform_overrides[]` entry whose platform does not canonicalise | `find_platform_override` never matches it, so the override silently does nothing | Preserved verbatim; surfaced as unresolved, with the offending name |
| `priority_overrides` naming an unknown ID | filtered out silently | Preserved verbatim; surfaced as unresolved |

Requirements:

- The loader keeps unrecognised entries in an explicit "unresolved" collection
  rather than discarding them, and the writer re-emits them unchanged.
- Unresolved entries never affect resolution — they are inert until the user
  fixes them — but they are always visible (GUI §3.12).
- A round-trip test loads a file containing all three cases, mutates an
  unrelated source, saves, and asserts the unknown entries survive byte-for-byte.

**Why this must precede the schema work.** A migration is precisely the moment
when a file is most likely to contain keys the running binary does not
recognise: a user who downgrades, upgrades, or hand-edits will have exactly that
file. Opening the schema while writes still silently drop unrecognised content
means the migration itself could destroy user preferences — the failure mode the
whole "no silent behaviour change" principle exists to prevent. It is also
independently valuable and independently revertible, so there is no reason to
sequence it later.

This fix adds **no new format fields**. It changes only what the writer does
with content it already reads.

### 3.1 Policy document format version

**Starting position: the shipped file has no version field and rejects unknown
keys.** `CheatSourcesConfig` is `#[serde(deny_unknown_fields)]`, so a released
binary refuses to parse a file containing any key it does not know — including
`format_version` itself. Adding the version field is therefore the *breaking*
step, and it can only be taken once.

Required sequence, and it cannot be reordered:

1. **Ship a tolerant reader first.** Release a version that ignores unknown
   top-level keys (or reads `format_version` when present and treats its absence
   as version `0`) while still writing the current format. Until this release is
   widely adopted, no new key may be written.
2. **Only then start writing `format_version = 1`** plus any new field.
3. A user who downgrades to a binary older than step 1 will find the file
   unreadable. The failure must be a clear "this preferences file was written by
   a newer EmuWiz" message with the path, and the binary must **fall back to
   built-in defaults rather than deleting or rewriting the file**.

> REVIEW FIX — the original §3 described versioning a document that was assumed
> not to exist yet. It does exist, unversioned and strict, so the version field
> cannot simply be introduced: doing so in one step would break every already
> released binary against a file it previously read fine. The two-release
> sequence above is the only way to add it without a silent config break.

Once `format_version` (a `u32`) exists, starting at `1` for the first versioned
document, the rules are:

- **Bump on any semantic change**: adding/removing/renaming a field, changing a
  default, changing resolution rules, or changing allowed values.
- **Non-breaking additions** (a new field with a default) may keep the same
  major version but must still be readable by an older reader via the
  unknown-field rule (§4.1). In practice, every model change in this design
  bumps the version to keep the equivalence guarantee simple and testable.
- **Downgrade safety**: a reader that only understands version N can still read
  a version N+1 document if and only if the unknown-field rule (§4.1) holds;
  otherwise the reader must refuse to open it rather than guess.
- The version is stored **inside** the policy document (not only in the filename
  or a sidecar) so a renamed file cannot masquerade as a different version.

### 3.2 Relationship to existing version counters

The new policy version is separate from the existing counters it coexists with:

- `CheatSourceManifest.format_version` (cheat-source manifests, currently 3;
  legacy 1–2 still readable) — unchanged by migration.
- `CHEAT_SOURCE_RESULT_SCHEMA_VERSION` (fetch/inspection result JSON) —
  unchanged; adding policy fields to results would be an additive schema change
  and must be reviewed separately.
- `CHEAT_SOURCE_LEGACY_SCHEMA_VERSION` — remains the floor for reading old
  manifests.
- Library DB `schema_version` — untouched; the policy document is not stored in
  the EmuWiz library database (model §1.1).
- `cheat_sources.toml` — **currently unversioned**; gains `format_version` only
  via the two-release sequence above.

### 3.3 Policy-document location and atomicity

- Stored at `~/.config/archivefs/cheat_sources.toml`, never inside the library
  database.
- **DECISION (approved): preference writes gain durable atomic-write
  semantics.** Today `atomic_write_text` does temp file + `rename` **without any
  flush**, which is weaker than the cheat-source `metadata.json` and manifest
  writers, which already `sync_all` (audit §3.2). The required sequence is:

  1. write the temp file;
  2. `sync_all` the temp file (contents durable before it is published);
  3. `rename` temp → target;
  4. sync the **parent directory** so the rename itself is durable, **where the
     platform supports it**.

  Step 4 is best-effort by platform: on Unix, open the parent directory and
  `sync_all` it; on platforms without directory-fsync semantics, skip it rather
  than failing the write. A skipped directory sync is not an error and must not
  be reported as one.

  Rationale: without step 2, a crash can publish a *truncated or empty* file
  under the real name — worse than losing the edit, because an empty file still
  parses as "defaults" and silently discards every preference the user had.
  Without step 4, the rename itself can be lost. Both are cheap to fix and the
  write is infrequent (only on an explicit user edit), so there is no throughput
  argument against it.

  This change is behaviour-preserving in every non-crash case and can ship on
  its own, ahead of any schema work.

- Every successful write records `format_version` once it exists; the previous
  generation is retained as the rollback target (§5).

---

## 4. Backwards compatibility

### 4.1 Reading an older policy document

- **Missing fields** are filled from the safe-default table for that version.
  Because every safe default reproduces today's behaviour, an old document that
  predates a field resolves to today's behaviour for that field.
- **Unknown fields** (document newer than reader) are ignored only when they
  cannot change interpretation of known fields; otherwise the reader refuses to
  open the document. The safe rule for the first implementation: a reader opens
  only documents it understands, and the document's `format_version` gate is
  explicit.
- **Duplicate or conflicting keys** in one scope are rejected at parse, never
  resolved silently (model §15.6).

### 4.2 Reading existing cheat-source state after migration

- `metadata.json`, manifests, snapshots, and pins are **not rewritten** by
  migration. The model reads them through the existing code paths and layers
  policy on top.
- Legacy manifest schema 1–2 remain readable and verifiable (existing
  `supported_cheat_source_schema` behaviour is preserved; the empty
  `canonical_repository_url`/`resolved_revision` fields stay empty rather than
  being invented).
- A snapshot without a policy document (e.g. produced by an older binary, then
  opened by a newer one) still verifies and is usable; the policy layer treats
  it with the safe defaults.

### 4.3 GUI/CLI parity

Both front ends resolve policy through the shared core resolver (model §15.8).
A CLI invocation and a GUI render of the same context must show the same
resolved values; the migration's test list includes a parity check for every
mapped field (audit §5).

---

## 5. Safe rollback

### 5.1 What rollback means

Two distinct rollback cases:

1. **Policy rollback** — reverting to a previous policy document after a user
   edit or after a migration. This is the case this section specifies.
2. **Cheat/install rollback** — the existing journal-driven install rollback. It
   is untouched by this design and is out of scope here.

### 5.2 Policy rollback procedure

- The previous policy document version is retained (at least one generation, in
  the same atomic-write directory). "Rollback available" means exactly that the
  prior generation is present and validates.
- Rolling back restores the prior `format_version` and its full content. Fields
  added by the reverted version are dropped; their effect disappears because
  resolution is a pure function of the document (model §15.4).
- **Runtime state and provenance are never rolled back.** `SourceRuntimeState`,
  cache, snapshots, manifests, and audit history survive a policy rollback. A
  reverted policy does not unpin a snapshot, delete a cache, or erase history.
- Rollback is itself a normal policy write: it goes through the same atomic
  write and records the change in provenance.
- A rollback that would make a *newer* document's runtime state meaningless is
  refused only if the newer state violates the reverted policy's validation
  (e.g. reverting a `Pinned` revision whose pin the current snapshot no longer
  matches). In that case the user is told what will stop working, not silently
  rolled.

### 5.3 Downgrade (binary) rollback

If a user reinstalls an older EmuWiz binary:

- The older binary reads the library/cache as before; the policy document is
  simply a file it does not read. This is safe **only if** the newer binary
  never relied on the policy document being required. The design therefore
  keeps the policy document optional: absence of the file must produce exactly
  the current default behaviour.
- The older binary must not be handed a policy document it cannot parse in a
  way that breaks it; because the document is opt-in for behaviour, an old
  binary ignoring it is fine, and a new binary encountering an old document
  applies §4.1.

### 5.4 Failure during migration

The first migration is a two-step write:

1. Build the new policy document from current state (pure function; no side
   effects).
2. Validate it (schema + resolution self-consistency, model §15.7) and write it
   atomically.

If step 2 fails, nothing is written; the previous state remains in effect and
the operation is reported. There is no partial policy document because the write
is atomic.

---

## 6. No silent behaviour changes

### 6.1 Definition

A **silent behaviour change** is any observable difference in an operation that
the user did not cause by changing a policy value. The migration's rule: the
safe defaults (§2, model §4–§12, GUI §0.2) must reproduce today's behaviour for
every operation that exists today.

### 6.2 Behaviour-equivalence checklist (must hold before/after migration)

For each item, the pre-migration and post-migration outcomes must be identical
when the policy document is at default values:

1. `retroarch-cheat-source-list` output (status labels, trust classification,
   freshness) — unchanged.
2. `retroarch-cheat-source-fetch` with no flags — resolves master, downloads,
   verifies, publishes, reuses fresh snapshot — unchanged.
3. `retroarch-cheat-source-inspect` — unchanged.
4. `--force-refresh`, `--offline`, `--expected-sha256`, `--max-download-bytes`,
   `--cache-root` — same semantics, same failure codes.
5. Snapshot `list`/`verify`/`pin`/`unpin`/`prune` — unchanged.
6. Cache lock acquisition, timeout, symlink refusal — unchanged.
7. Manifest schema handling (1/2/3) — unchanged.
8. `dat inspect` / `validate` / `audit` — unchanged (DATs untouched by the
   cheat-source migration).
9. GUI Sources page and Cheats & Mods catalogue manager states and labels —
   unchanged.
10. Provenance records produced by fetch — unchanged fields, plus additive new
    fields only.
11. `cheat-source list` — same nine entries, same order, same enabled states.
12. `cheat-source info <id>` — unchanged for every ID.
13. `cheat-source enable | disable | set-priority` — same accepted range
    (`1..=999`, rejected not clamped), same persisted file, same output.
14. An **absent** `cheat_sources.toml` still means "all nine enabled at default
    priority"; a **present** one is read without being rewritten.
15. A file containing an unknown provider ID or an unresolvable platform name is
    not silently emptied of it (audit §3.2) — this is a *bug fix*, so it is the
    one permitted deviation and must be called out in release notes rather than
    hidden inside the migration.

### 6.3 Fields that intentionally change nothing at default

The following new fields are defined so that their default is behaviourally
inert; they only act when changed:

- `priority` — **already live, and not inert.** Sources ship with distinct
  default priorities (10 … 100) and ordering is already observable through
  `cheat-source list` and `sorted_enabled_for_platform`. The migration guarantee
  here is narrower and must be stated exactly: *the model must not change any
  source's default priority, and must not invert the comparison.* Lower still
  wins. Anything else silently reorders which source answers first for every
  existing user.
- `region_preferences` / `language_preferences` (default empty) — no effect.
- `clone_policy` (default `Standalone`, DAT block only) — identical to today's
  DAT audit, which ignores parent relationships.
- `verified_only` (default `false`) — identical to today's full evidence
  hierarchy.
- `conflict_policy` (default `ReportAll`) — identical to today's collision and
  candidate reporting.
- `rename_safety` — fixed at `NeverSuggest`, the only implemented value
  (model §12.2). Identical to today: no renames, ever. Not a field the user can
  change, and not exposed as a control.
- `variant_policy` (default `PreferCanonical`, cheat block only) — identical to
  today's presentation order.
- `trust_level` (default `BuiltInReviewed` for all nine built-ins) — a new label
  on existing sources; grants no new capability and relaxes no check. It
  describes the integration only and asserts nothing about upstream content
  (model §3.1).

### 6.4 How the guarantee is enforced

- **Safe-default table** lives in core beside the resolver; the GUI's
  "Differs from default" marker and the migration tests both read it.
- **Equivalence tests** are added at the core layer (not just CLI) so both
  front ends inherit the guarantee. They are described here for the future
  implementation, not written now:
  - a test that resolves every existing operation's inputs under the default
    policy document and asserts equality with the pre-migration result;
  - a test that an absent policy file behaves identically to a default policy
    file;
  - a parity test that CLI and GUI render the same resolved values for the same
    context.
- **No heuristic migration.** Nothing is inferred (shell history, filenames,
  directory layout) into the policy document. User-added DAT paths are never
  auto-registered (§2.5).

---

## 7. Migration steps (implementation order, not code)

Ordered so that each step ships and reverts on its own, that data-loss fixes
precede anything that rewrites the file, and that the one irreversible step (the
schema opening) is taken only after the ground is safe.

**Milestone 1 — GUI for the existing nine cheat sources. No new format fields.**

1. **Lossless round-trip fix** (§3.0). Preserve unknown provider IDs and
   unresolved platform overrides on write; surface them as unresolved. Adds no
   fields; strictly stops losing user data. Prerequisite for everything below.
2. **Durable preference writes** (§3.3): temp → `sync_all` → rename → parent
   directory sync where supported. No format change.
3. **GUI for the nine cheat sources**: list (GUI §3.10), enable/disable
   (§3.6), priority (§3.11), per-platform participation (§3.11). Reads and
   writes `cheat_sources.toml` in its existing shape.

Milestone 1 ends here. At this point `cheat_sources.toml` is still byte-
compatible with every released binary, so all of milestone 1 reverts cleanly by
reverting the code.

**Milestone 2 — open the schema.**

4. **Ship the tolerant reader** (§3.1 step 1). No behaviour change. Must be
   released and adopted before step 5.
5. **Open the schema**: write `format_version = 1`.

**Milestone 3 — new policy fields.**

6. Add `trust_level` and verification fields to `CheatSourceSpec`, defaulted to
   today's behaviour, rendered with the scope wording required by model §3.1.
7. Add the shared, behaviour-neutral policy helpers (URL/DNS policy, cache-root
   path policy) without changing observable behaviour.
8. Add the Effective Policy Summary (read-only), then the remaining
   inert-at-default matching fields.
9. Add DAT source registration and the DAT priority space (model §5.2), behind
   the "register explicitly" rule (§2.5).
10. Add review-queue additions (excluding the rename review, which is not
    implemented — GUI §5.3).
11. Extend the equivalence suite at each step.

Steps 1–3, 4, and 6–11 are independently shippable and independently reversible
(§5). **Step 5 is not reversible** in the compatibility sense: once a version key
is written, binaries predating step 4 cannot read the file. It is called out
separately for that reason.

> DECISION (approved) — milestone 1 is the first implementation target, and it
> deliberately adds no persistence format fields. It converts the nine already-
> shipped cheat sources from CLI-only to user-visible and controllable, which is
> the largest user-control gain available, while leaving the on-disk format
> untouched and every step revertible.

> REVIEW FIX — the original order began by building shared model types and
> writing a new policy document, and described every step as independently
> reversible. With a strict, unversioned file already deployed, the schema
> opening is a one-way door that must be sequenced behind a tolerant reader and
> behind the round-trip fix.

---

## 8. Summary

The migration keeps every existing cheat-source setting, cache, manifest, pin,
and CLI flag working identically by mapping them onto the new model at their
current values, adds the new policy fields only at safe defaults, and grows the
already-shipped `cheat_sources.toml` rather than introducing a second source of
truth. It guarantees rollback at two levels: a one-generation policy rollback,
and a binary downgrade that ignores the optional file — the latter only for
binaries at or after the tolerant-reader release (§3.1). The acceptance test for
the whole migration is the behaviour-equivalence checklist (§6.2): if a default
policy document changes an existing operation's observable outcome, the
migration is wrong.

---

## 9. Decisions of record

### 9.1 Approved and applied

All six previously-open decisions are settled. Each is recorded in full at the
section listed; this table is the index.

| # | Decision | Recorded in |
| --- | --- | --- |
| 1 | **DAT and cheat sources use separate priority spaces.** They answer different questions and are never compared. | model §5.2; GUI §1.6, §3.11 |
| 2 | **DAT priorities are ordered only against other DAT sources relevant to the same platform.** Ordering is computed per platform over the relevant subset; sources covering disjoint platforms never compare. Default `100`. | model §5.2, §5.5; §15.3 |
| 3 | **Shipping a scraper or provider does not mean EmuWiz reviewed or endorsed the upstream content.** Trust describes the integration only, and the label always carries that scope. | model §3.1, §3.2, §3.5; GUI §3.5 |
| 4 | **The shipped lower-number-wins cheat priority rule is preserved** for compatibility, and not inverted. The GUI renders resolved position rather than the bare number. | model §5.1; GUI §3.11 |
| 5 | **Rename safety remains `NeverSuggest` for the first implementation.** The other modes are future design only and must not appear as active GUI controls — not even disabled ones. | model §12; GUI §2.6, §5.3 |
| 6 | **Preference writes gain durable atomic-write semantics**: file `sync_all`, then rename, then parent-directory sync where the platform supports it. | migration §3.3 |
| 7 | **Unknown provider IDs and unresolved platform overrides round-trip without data loss**, as a prerequisite bug fix before any schema migration. | migration §3.0; GUI §3.12 |
| 8 | **First implementation milestone is the GUI for the existing nine cheat sources** — list, enable/disable, priority, per-platform participation — adding no new persistence format fields. | migration §7; GUI §3 preamble |

Decisions 1, 2 and 4 together resolve what was previously the largest open
question: there is no cross-space priority band to choose, because there is no
cross-space comparison.

### 9.2 Still open

These are genuinely undecided and are **not** required for the milestones above.

1. **Whether DAT sources ever need per-platform priority overrides.** Decision 2
   makes DAT priority platform-local already, so an explicit per-platform
   override may be redundant. Defer until a real two-DAT-per-platform case
   exists.
2. **Whether `BuiltInReviewed` is the right name** now that its meaning is
   scoped to the integration (decision 3). The rendering is fixed and correct;
   the identifier may still mislead a future reader of the code. Renaming it is
   cheap before the field ships (milestone 3, step 6) and expensive after.
3. **Whether rename safety is ever implemented at all.** Decision 5 settles the
   first implementation, not the long term. `NeverSuggest`-forever remains a
   legitimate outcome.
4. **Whether DAT sources belong in `cheat_sources.toml`** or in their own file.
   The filename is cheat-specific; sharing it would be convenient but
   misleading. Decide before milestone 3, step 9.

---

*Document created: 2026-08-04*
*Baseline: docs/design/DAT_CHEAT_POLICY_AUDIT.md (approved); model:
docs/design/DAT_CHEAT_POLICY_MODEL.md; GUI: docs/design/DAT_CHEAT_POLICY_GUI.md.*
*No production code is modified by this document.*
