# DAT Sources — Stage 2: Matching Preferences and Effective Policy

Status: implemented on `feature/dat-preferences-policy`. This document describes
what shipped, not a proposal. It implements the parts of the approved
`docs/design/DAT_CHEAT_POLICY_*.md` documents that current `main` can support
without crossing the migration one-way door, and calls out every departure.

Stage 1 (`DAT_SOURCES_STAGE1_IMPLEMENTATION.md`) shipped the DAT source
registry, its schema-tolerant `dat_sources.toml`, and the read-only audit.
Stage 2 adds the user-controlled **DAT matching policy**: ordered region and
language preferences, revision and clone policies, platform-local source
priority used to *rank already-verified candidates*, per-platform overrides,
and the Effective Policy Summary. All file operations remain read-only.

---

## 1. What the policy answers

`archivefs_core::dat::policy` provides:

| Concept | Type | Safe default | Today's equivalent |
| --- | --- | --- | --- |
| Region preference | ordered `Vec<RegionId>` | empty (all equal) | audit reports every candidate |
| Language preference | ordered `Vec<LanguagePreference>` | empty (all equal) | audit reports every candidate |
| Revision policy | `RevisionPolicy` | `AskWhenAmbiguous` | audit never picks a revision |
| Clone policy | `ClonePolicy` | `KeepAllVariants` | audit ignores parent relationships |
| Source priority | persisted `u32` | `100`, lower wins | platform-local ordering |
| Per-platform participation | platform-scoped resolution | all enabled | `sorted_enabled_for_platform` |

The policy only ever **ranks candidates the audit already verified** and
**explains the rank**. It cannot promote a weaker-evidence candidate, cannot
weaken a verdict, and never resolves genuine ambiguity silently. Rename safety
remains `NeverSuggest`: there is no rename plan type and no rename control
anywhere.

## 2. Where the preferences live

`~/.config/archivefs/dat_sources.toml` owns the DAT preferences, in a `[policy]`
table. It is the same file that owns the DAT source registry, so there is one
answer to "what is this user's DAT matching policy" and one durable-write path.
It is schema-tolerant exactly like the rest of that file:

- unknown keys in `[policy]` and in each per-platform override are captured by
  `#[serde(flatten)]` and re-emitted verbatim on save;
- preference *values* are stored as raw strings (`revision_policy =
  "latest_verified"`, region IDs, language IDs / `multi` / `original`) so a
  value a newer build could invent round-trips instead of failing the parse;
- there is deliberately **no `format_version`** — the file keeps what it does
  not understand, so a version key would have no consumer. This PR does not
  cross the migration one-way door (migration §3.1, §4).

```toml
[policy]
region_preferences = ["europe", "usa"]
language_preferences = ["en", "multi"]
revision_policy = "latest_verified"
clone_policy = "prefer_parent"

[policy.platforms.NES]
region_preferences = ["japan"]
```

Field semantics follow the design's `Option` rule (model §7.2): absent = use the
safe default / inherit the parent scope; present empty list = "no preference,
do not inherit" (only reachable by hand-editing; the GUI reverts an emptied
list to absent). Per-platform override keys must be canonical platform IDs; a
non-canonical key is preserved verbatim, reported as a validation problem, and
not applied.

## 3. Effective policy resolution

`EffectiveDatPolicy` is the resolved policy for one scope (global or a canonical
platform), merged field-by-field from the global document and the platform
override (model §15.2). It carries the sources that participate in that scope in
consultation order (`source_ordering`) and, per field, which scope supplied the
value (`scope_of`), which the Effective Policy Summary displays as "source of
value". Resolution, participation filtering, and validation all live in core;
the GUI renders their output and never re-implements policy logic.

Source priority is platform-local: candidates are filtered to participating
sources *before* ranking, so two sources covering disjoint platforms never
compare (test: `disjoint_platforms_never_compare_priorities`), and DAT priority
is never compared with cheat priority — there is no shared priority space (model
§5.2).

## 4. Candidate ranking

`rank_candidates(candidates, &EffectiveDatPolicy) -> CandidateResolution` ranks
an already-verified candidate set. The comparator applies, in order: source
priority (lower wins), clone handling under `PreferParent`, region, language,
revision, then clone handling under `PreferClone`. The first decisive step
explains itself. Results:

- `entries` — deterministic display order (ties broken by label);
- `decided` / `winner_index` — true only when the top entry strictly outranks
  every other; the deterministic display order is deliberately *not* a decision;
- `ambiguous` / `ambiguity_reason` — when the policy cannot separate the top
  candidates, nothing is picked (design rule: conflicts are never silently
  collapsed, model §11.3);
- `explanations` — e.g. `preferred region matched (Europe)`,
  `newer verified revision preferred (Rev 2)`,
  `source priority 20 outranked source priority 100`, `parent preferred`,
  `a clone and its parent are tied and the policy requires an explicit choice`;
- `excluded` — candidates whose source does not participate in the scope.

The audit integration (`dat::sources::audit_run`) carries the effective policy
in the request and annotates `ExactMultipleCandidates` verdicts with the
resolution. The verdict is never replaced — the note is additive and inert by
default.

## 5. Vocabulary choices

- **Regions.** The canonical set is `world`, `usa`, `japan`, `europe`, `other`
  (model §7's examples plus `Other`). Everything a catalogue name tags that is a
  region but is not one of the four maps to `Other` (e.g. `Germany`, `Brazil`,
  `Asia`). Growing the vocabulary later is additive.
- **Languages.** ISO 639-1 codes (`en`, `ja`, `de`, …). A language preference
  list may also contain `multi` (any entry with more than one language tag) and
  `original` (the release region's own language: USA/World/Europe → English,
  Japan → Japanese — a documented heuristic, see
  `original_language_of`).
- **Revision.** Read from `(Rev N)` / `(Rev A)` markers in names; no marker =
  the original dump (revision 0).
- **Parent/clone.** `DatGameEntry::clone_of` is now captured by both parsers
  (Logiqx `cloneof`/`cloneofid`, ClrMamePro `cloneof`/`romof`), so the clone
  policies act on real data. Safe default `KeepAllVariants` means parent
  relationships are ignored, exactly as the audit behaved before.

## 6. GUI

The DAT Sources page gains a **DAT matching policy** section (shown even with no
sources registered) and an **Effective policy** summary:

- scope selector ("All platforms" or one platform the sources cover);
- preferred regions: add / move up / move down / remove, with an "Add" row;
- preferred languages: add (via a picker) / move / remove, plus `multi` and
  `original` entries;
- revision policy and clone policy selectors;
- a fixed statement "ArchiveFS never renames your files" — no selector for the
  future rename modes, per design decision 5;
- the Effective Policy Summary: current platform, sources consulted in order,
  resolved region/language/revision/clone values, and "where each value comes
  from" (global vs platform override);
- validation problems in the persisted policy are shown as "kept as written"
  warnings, never silently dropped.

Audit results, when a policy is present, show a **Policy preference** note per
multi-candidate file: the ranked candidates, the reasons, the preferred winner
or the ambiguity.

All policy edits are unsaved changes like any other: they write to the draft
registry, dirty the page, and reach disk only on Save through the existing
durable atomic write.

## 7. Safety guarantees

- **No ROM is ever written.** The policy module's only I/O is the registry's
  own durable write; ranking and resolution are pure functions of their inputs.
- **No verdict is weakened.** Preferences rank already-verified candidates only.
- **No silent resolution.** A tie the policy cannot break stays ambiguous, with
  a reason.
- **No data loss.** Unknown keys and unknown preference values round-trip; the
  GUI surfaces them rather than dropping them.
- **No cross-space priority.** DAT priorities are compared only against other
  DAT sources that participate in the same platform.

## 8. Deferred (out of scope for this stage)

Per the design documents and this task's explicit deferrals:

- automatic rename / move / delete of ROMs (rename safety is `NeverSuggest`);
- per-source policy overrides (the GUI edits global + per-platform policy);
  source priority stays per-source but is inspect-only, as in Stage 1;
- trust levels for DAT sources (model §3) — requires a cheat-side schema step;
- cheat-side policy (the nine cheat sources) — unchanged by this PR;
- `format_version` and the migration one-way door (migration §3.1);
- network access of any kind.

---

*Created: 2026-08-07*
*Builds on: DAT_SOURCES_STAGE1_IMPLEMENTATION.md and the approved
DAT_CHEAT_POLICY_{AUDIT,MODEL,GUI,MIGRATION}.md design documents.*
