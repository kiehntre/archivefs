# DAT & Cheat-Source Policy Model

Status: design proposal. This document is built directly on the approved
baseline [`docs/design/DAT_CHEAT_POLICY_AUDIT.md`](DAT_CHEAT_POLICY_AUDIT.md)
and is one of three deliverables it specifies (this model document, the GUI
document, and the migration document). No Rust code is implemented here; this
document defines the data model a future shared policy layer must expose.

Scope: the model covers policy for both **DAT sources** and **cheat sources**
(and, by extension, any future source type registered through the same layer).
It deliberately does not cover parser grammar, catalogue matching internals,
installation/rollback transactions, or RetroArch/emulator-specific behaviour -
those are called out as non-shared in the audit (§7).

---

## 0. Design goals

1. **One vocabulary for both subsystems.** DAT sources and cheat sources are
   registered, trusted, enabled, prioritised, and resolved through the same
   policy document, even though their parsers and consumers stay separate.
2. **Safe defaults.** Every policy field has a default that reproduces today's
   behaviour. A user who never opens the policy UI gets exactly what the
   current build does (see the migration document).
3. **Deterministic effective-policy resolution.** For any (source, platform,
   region, language) triple there is exactly one resolved policy value, and the
   resolution rule is documented and testable.
4. **Runtime state is never policy.** What a source *did* (last fetch, last
   failure, current snapshot) lives in `SourceRuntimeState`, not in the
   user-authored policy document.
5. **No silent behaviour change.** Adding a field, removing a field, or
   changing a default must be a versioned, migratable event (see the migration
   document).

---

## 1. Core vocabulary

### 1.1 Policy document

A **policy document** is the single, user-editable container of every source
policy described in this document.

**It is not a new file.** The policy document is the already-shipped
`~/.config/archivefs/cheat_sources.toml` (`CheatSourcesConfig`), grown to carry
the additional fields this model defines. Today it holds
`(providers[], platform_overrides[])`; this model adds `global_defaults`,
`platform_defaults`, and the per-source matching fields (§7–§13).

- One policy document exists per user (it is not per-library or per-worktree).
- It is stored outside the ArchiveFS library database, next to the existing
  config, and is written **durably and atomically**: temp file, `sync_all`,
  `rename`, then parent-directory sync where the platform supports it
  (migration §3.3).
- It is read-only to consumers: policy is loaded once per operation and passed
  down; nothing below the loader mutates it.
- Only non-default values are persisted, as today: an absent or empty file means
  "every built-in source enabled at its default priority".

**Hard constraint — the file has no version field and rejects unknown keys.**
`CheatSourcesConfig` is `#[serde(deny_unknown_fields)]`. Adding *any* key,
including `format_version` itself, makes the file unreadable by every already
released binary. Every field this model adds is therefore gated on the
one-time, explicitly previewed schema step described in the migration document
(§3.2, §4.1). No field in §7–§13 may ship before it.

> REVIEW FIX — this section originally described a brand-new versioned policy
> document. PR #2 shipped a per-user preferences file that already stores
> enabled state, priority, and per-platform overrides. Two files answering "is
> this source enabled?" is a correctness hazard, so the model now extends the
> shipped file. The `deny_unknown_fields` constraint is stated here because it
> governs whether any of this design can ship at all.

### 1.2 Source

A **source** is a registered, addressable origin of catalogue data. Two concrete
kinds exist today:

- **DAT source** - a local DAT file or DAT directory (Logiqx XML or ClrMamePro
  text). Today DATs are user-typed CLI paths with no registry; the model adds
  registration so they can carry the same policy as cheat sources.
- **Cheat source** - one of the nine registered cheat sources
  (`build_default_registry()`), spanning RetroArch, PCSX2, Dolphin, Xenia, and
  the cross-platform BSFree archive. Only `libretro-buildbot-cheats` uses the
  immutable-snapshot download pipeline; the others differ in what they can do,
  which is described by `CheatSourceCapabilities`, not by this policy model.

A source has a stable **identity**, a **trust level**, an **enabled state**, a
**global priority**, and an ordered set of **policy overrides** scoped by
platform. It also references its last-known **verification configuration** and
its **runtime state**.

### 1.3 Effective policy

The **effective policy** for a given lookup is the merged result of:

```
global_defaults
  -> source (global-priority / source-level settings)
    -> platform_defaults[platform]
      -> source.platform_overrides[platform]
```

The merge is monotonic in confidence: more specific scope wins, and fields that
are not overridden at a given scope fall through to the less specific scope.
Resolution is described fully in §15.

---

## 2. Source identity

### 2.1 Definition

`SourceId` is an opaque, stable, non-empty string that identifies one source for
its entire lifetime.

### 2.2 Requirements

- **Stable.** Never derived from a mutable URL, a display name, or a moving
  branch reference. For the compiled-in cheat source the ID stays
  `libretro-buildbot-cheats`.
- **Opaque.** The ID is a label, not a capability. No code ever treats the ID
  as a path component, a URL, or an executable.
- **Unique.** The registry forbids two sources with the same ID.
- **Displayed honestly.** The ID may be shown to users, but the human-readable
  name (`display_name`) is separate and can change without changing identity.
- **Not a security boundary by itself.** Identity stability preserves audit
  continuity; it does not transfer trust. Changing a source's scheme, origin,
  base path, or verification material requires re-review (see trust level).

### 2.3 Allowed values

- ASCII letters, digits, `-`, `_`, `.`; 1–128 bytes.
- Must not start or end with `.`, must not be `.` or `..`, must not contain
  path separators.

### 2.4 Safe default

None - a source has no implicit ID. DAT sources get a user-chosen ID at
registration; cheat sources keep their compiled-in ID.

### 2.5 Plain-language effect

Shown to the user as: *"The stable name ArchiveFS uses to remember this source.
It won't change if the download location changes."*

---

## 3. Trust level

### 3.1 Definition

`SourceTrustLevel` describes how the source's **integration** — its origin,
endpoints, transport, format, and limits — was vetted. It reuses and generalises
the existing three-state model from `docs/CHEATS_MODS_USER_POLICY.md`
(Trusted / Unverified / Blocked) but keeps the Blocked *content* state separate
from *source* trust (a structurally unsafe file does not change a source's trust
level; see §7 of the audit).

**DECISION (approved): shipping a provider does not mean ArchiveFS has reviewed
or endorsed the upstream content.** Trust is a statement about the *pipe*, never
about what flows through it. ArchiveFS reviewed the URL policy, the host, the
transport, the parser, and the resource ceilings for a built-in source. It has
not reviewed, curated, or endorsed the cheats, patches, or catalogue entries the
upstream publishes, and it makes no claim that they are correct, safe to apply,
or lawfully redistributable.

This distinction is load-bearing for the scraper-backed entries
(`gamehacking.org-*`, `bsfree-archive`, the Dolphin and Xenia upstreams): those
carry community-submitted content that no ArchiveFS maintainer has read.

### 3.2 Values

| Value | Meaning — always about the integration, never the content | Today's equivalent |
| --- | --- | --- |
| `BuiltInReviewed` | ArchiveFS maintainers reviewed this source's origin, endpoints, format, and limits, and ship it. **No claim is made about the upstream's content.** | `trust_status = "built_in_reviewed"`, reported by `list_retroarch_cheat_sources()` for the one retrieval source only. |
| `UserTrusted` | A user explicitly reviewed and elevated this source's origin. | None yet (future user-added sources). |
| `Untrusted` | The integration has not been reviewed. Community content, friend-shared files, local files. | The planned `LocalUnverifiedSource`/`RemoteUnverifiedSource` GUI cards. |

**This is a new field, not a mapping.** `CheatSourceSpec` has no trust field
today, so the other eight registry entries have no trust level to migrate. All
nine compiled-in entries adopt `BuiltInReviewed` when the field is introduced —
under the scoped meaning above, this is simply the true statement that ArchiveFS
reviewed and ships the integration.

**Naming consequence.** Because `BuiltInReviewed` is easy to misread as
"ArchiveFS vouches for these cheats", the label must never be rendered bare. The
GUI and CLI render it with its scope attached — "Built-in integration; upstream
content not reviewed" — and never as an unqualified "Reviewed" or "Trusted"
badge (GUI §3.5).

### 3.3 Rules

- **Sources start untrusted.** Every user-added source begins as `Untrusted`.
  Prior success does not raise trust.
- **Trust is raised only by explicit local action.** There is no automatic
  promotion. Passing a structural check makes a file *structurally valid*, not
  *trusted*; the two claims are distinct.
- **Trust is downgraded on material change.** If a `BuiltInReviewed` or
  `UserTrusted` source changes scheme, origin, base path, authentication, or
  verification material, it is disabled and must be re-reviewed while its ID is
  retained for audit continuity.
- **`Blocked` is not a trust level.** A source whose *content* is unsafe at a
  given revision is marked `Blocked` on that revision's outcome, never on the
  source's trust field.
- **Trust never gates content checks.** Every structural, path, digest, and
  archive check runs identically at every trust level. Raising trust changes a
  label and which review prompts appear; it never relaxes a check. This follows
  directly from the decision above: since trust says nothing about content, it
  cannot be a reason to inspect content less.

### 3.4 Safe default

`Untrusted` for any newly registered source; `BuiltInReviewed` for the nine
compiled-in cheat sources, under the integration-only meaning defined in §3.1.

### 3.5 Plain-language effect

Shown as: *"Built-in integration"* / *"You reviewed this source"* / *"Not
reviewed yet"*, each with its scope attached. The user is told:

*"ArchiveFS reviewed how this source is fetched and parsed — the address, the
transport, and the limits. It has not reviewed the cheats or patches the source
publishes, and does not endorse them. Codes come from the upstream community;
check anything you apply."*

And for unreviewed sources: *"Not reviewed means ArchiveFS's developers haven't
checked this source's setup. It isn't a judgement that the source is bad."*

---

## 4. Enabled state

### 4.1 Definition

`enabled: bool` - whether the source participates in catalogue scans, audits,
and retrieval.

### 4.2 Rules

- A disabled source is ignored by all read paths and all fetch paths.
- Disabling does not delete its runtime state, cache, or audit history.
- Disabling is reversible and instant; no confirmation beyond the normal UI
  confirmation is required because it writes nothing.
- A source with an unsatisfied trust/verification requirement is reported as
  disabled-with-reason, never half-enabled.

### 4.3 Allowed values

`true` | `false`.

### 4.4 Safe default

- Cheat source: `true` for **all nine** registered sources
  (`CheatSourceEntry::from_spec` sets `enabled: true`). A disabled source is
  persisted as an explicit `enabled = false` entry; an absent entry means
  enabled.
- DAT source: `false` for newly registered, unverified sources; `true` only
  after explicit review or when it is a local file the user chose knowingly.
  Note this deliberately differs from the cheat-source default, because a DAT
  source is a user-supplied path rather than a reviewed built-in.

### 4.5 Plain-language effect

*"Enabled sources are scanned and can be downloaded from. Disabled sources are
kept but not used."*

---

## 5. Priority

### 5.1 Definition

`priority: u32` - an ordering of sources used when more than one source can
provide the same entry. **Lower values win** (priority `1` is consulted before
priority `999`); equal values are resolved deterministically by `SourceId`
(lexicographic) so ordering never depends on iteration order.

**DECISION (approved): cheat priority keeps the shipped lower-number-wins
rule.** It is not inverted. Inverting it would silently reorder which source
answers first for every existing user and would require a config migration to
fix. The convention reads backwards to some users, so the GUI must always render
the ordering explicitly ("consulted 1st, 2nd, 3rd…") rather than exposing the
bare number as if it meant importance.

> REVIEW FIX — this section previously specified `i32`, default `0`, "higher
> values win". The shipped registry uses `u32` in `1..=999`, sorts *ascending*
> (`sorted_all`, `sorted_enabled`, `sorted_enabled_for_platform` all use
> `a.priority.cmp(&b.priority)`), and assigns distinct non-zero defaults. The
> model was corrected to match the code; the decision above makes that
> permanent.

### 5.2 Two separate priority spaces

**DECISION (approved): DAT sources and cheat sources have separate priority
spaces. They are never compared to each other.**

| Space | Members | Question it answers |
| --- | --- | --- |
| Cheat priority | the nine registered cheat sources (and future cheat sources) | "Which catalogue provides the cheat content for this game?" |
| DAT priority | registered DAT sources only | "Which catalogue is authoritative for this game's *identity*?" |

The two answer different questions, so a single ordering would be meaningless: a
DAT source ranked `20` is not "more important" than a cheat source ranked `30`,
because they never compete for the same claim. There is no cross-space
comparison, no shared numbering, and no need to reserve bands. Each space is
ordered independently by the same comparator (lower first, `SourceId` tie-break).

Consequences:

- A DAT source's priority number may freely coincide with a cheat source's; the
  two are never sorted into one list.
- The resolver takes the space as part of the lookup key (§15.3).
- The GUI presents them as two separate ordered lists and must never render a
  single merged ordering (GUI §1.6, §3.11).

**DECISION (approved): DAT priorities are ordered only against other DAT sources
that are relevant to the same platform.** A DAT source is "relevant" to a
platform when it is enabled, participates in that platform (§6.2), and its
catalogue covers that platform. Ordering is therefore computed per platform over
the relevant subset, not once globally:

- Two DAT sources covering disjoint platforms never compare, whatever their
  numbers. A No-Intro NES DAT and a Redump PS2 DAT do not compete and their
  relative priority is not a meaningful question.
- A DAT source's priority only has an observable effect where two or more
  relevant DAT sources overlap on one platform.
- This makes DAT priority locally meaningful and removes any need for a global
  DAT ordering, or for a default priority chosen relative to the cheat band.

### 5.3 Purpose

When two DAT sources relevant to the same platform both claim a game, priority
decides which claim is authoritative for matching and audit *before* any
evidence-quality comparison is made. Priority is a tie-breaker, never a
substitute for evidence quality: a lower-priority exact hash still beats a
higher-priority filename guess. Confidence rules (§10, §11) always dominate
priority.

### 5.4 Allowed values

An unsigned integer in `1..=999`, in both spaces. Two distinct behaviours
already ship for cheat sources and must be preserved:

- A value supplied on the **CLI** is *rejected* if out of range, never clamped
  (`cheat-source set-priority x 5000` fails rather than silently confirming
  `999`).
- A value read from the **config file** is *clamped* into range by
  `apply_config`, because refusing to start over a hand-edited file would be
  worse than correcting it. The clamp is reported, not silent.

### 5.5 Safe default

**Cheat space.** Each source keeps its compiled-in `default_priority`. These are
not all `0`; they are deliberately spaced:

| Source | `default_priority` |
| --- | --- |
| `libretro-buildbot-cheats` | 10 |
| `pcsx2-official-patches-tree` | 20 |
| `gamehacking.org-ps2` | 30 |
| `gamehacking.org-gamecube` | 40 |
| `gamehacking.org-wii` | 50 |
| `dolphin_upstream_gamesettings` | 60 |
| `dolphin_upstream_catalogue` | 65 |
| `xenia_canary_game_patches` | 70 |
| `bsfree-archive` | 100 |

**DAT space.** A newly registered DAT source defaults to `100`. Because DAT
priority is only ever compared against other DAT sources relevant to the same
platform (§5.2), the absolute value carries no cross-space meaning and the
default only has to leave room to move a source earlier or later. A user with a
single DAT source per platform never needs to touch it.

### 5.6 Validation

Must be an unsigned integer in range (rejected on the CLI, clamped on load).
Ties are **not** an error: they resolve deterministically by `SourceId`. No
platform override is required to break a tie.

Cross-space comparison is a **programming error**, not a validation error: the
resolver takes the space as part of its key, so a DAT priority and a cheat
priority can never reach the same comparator.

> REVIEW FIX — the original text required a platform override whenever two
> sources tied for a platform. That validation is both unnecessary (the
> `SourceId` tie-break is already total and deterministic) and unimplementable
> as stated, since the check would have to run over every platform on every
> save. Removed.

### 5.7 Scope

Per space. Within a space: global by default, overridable per platform. DAT
priority is only *observable* per platform (§5.2).

### 5.8 Plain-language effect

*"Sources are consulted in order, lowest number first. DAT sources and cheat
sources are ordered separately — they answer different questions and are never
ranked against each other. Actual hash matches always beat a name guess, no
matter which source is preferred."*

---

## 6. Per-platform overrides

### 6.1 Definition

`platform_overrides: Map<PlatformId, PlatformPolicyOverrides>` - a per-platform
subset of policy fields that overrides the source's global settings for that
platform only.

### 6.2 Which fields are overridable

Overridable: priority, **per-platform participation**, region preferences,
language preferences, revision policy, clone/parent policy, verified-only flag,
duplicate/conflict handling, and cheat variant policy.

Not overridable: identity and trust level. A source is trusted or not
everywhere; trust describes the origin, which does not vary by platform.

**Per-platform participation already ships.**
`PlatformOverrideEntry.disabled_providers` removes a provider from
`sorted_enabled_for_platform` for one platform, and
`PlatformOverrideEntry.priority_overrides` re-prioritises it. These are distinct
from the source-level `enabled` flag and both must be modelled:

| Concept | Field | Meaning |
| --- | --- | --- |
| Source-level enabled | `ProviderConfigEntry.enabled` | Off everywhere, for every read and fetch path. |
| Per-platform participation | `PlatformOverrideEntry.disabled_providers` | Source stays enabled, but does not contribute for this platform. |

A source that is disabled at source level is off for every platform regardless
of platform overrides; a platform override can subtract participation but never
add it back.

> REVIEW FIX — the original text asserted that enabled state is never
> per-platform. `disabled_providers` has shipped since PR #2 and does exactly
> that. Rather than contradict the code, the model now names the two states
> separately so the GUI can present them without conflating them.

### 6.3 Rules

- Platform IDs are normalised through `canonical_platform_for_alias`, so aliases
  are accepted and stored values round-trip.
- **An unresolvable platform ID must be reported, not ignored.** Today
  `find_platform_override` simply never matches an override whose platform
  string does not canonicalise, so a typo silently disables the user's entire
  override. Validation must surface this at load time.
- **An unknown provider ID must be reported and preserved.** Today
  `apply_config` skips unknown IDs and `to_config()` re-serialises only live
  registry entries, so the next write **deletes** the user's line without
  warning. The policy layer must round-trip entries it does not recognise, or
  refuse to save until the user resolves them.
- An override covers only the fields present in the override map; absent fields
  fall through to the source/global scope (see §15).
- Overriding a field to a value that contradicts a non-overridable field is a
  validation error (e.g. "verified only on platform X" is fine; "untrusted on
  platform X" is not).
- When several overrides name the same canonical platform, the **last** wins
  (`find_platform_override` searches in reverse). Duplicate platform entries
  should be rejected at validation rather than relying on this.

### 6.4 Safe default

Empty map (no overrides), which is today's default state. Note per-platform
overrides are already *supported*; they are simply unset unless the user hand-
edits the TOML, since no CLI or GUI writes them.

### 6.5 Plain-language effect

*"Settings that only apply to games on [platform]. Everything else still uses
the source's global settings."*

---

## 7. Region and language preferences

### 7.1 Definition

`region_preferences: Vec<RegionId>` and `language_preferences: Vec<LanguageId>` -
ordered preference lists used to choose between multiple catalogue entries that
otherwise match equally well (e.g. several regional releases of one game).

### 7.2 Rules

- Order matters: the first entry in the list that a source can satisfy wins.
- An empty list means "no regional/language preference; treat all as equal".
- A region/language preference never promotes a weaker evidence class over a
  stronger one. It only disambiguates within the same evidence tier.
- Unknown region/language IDs are rejected at validation (region IDs come from
  the same registry used by platform/game identity).
- A field that is **absent** at a scope inherits; a field present as an **empty
  list** means "no preference, do not inherit". This is the ordinary
  `Option<Vec<_>>` distinction the shipped config already uses
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`), so it needs no
  extra vocabulary.

> REVIEW FIX — an extra `RegionPreference::Any` sentinel was specified here to
> distinguish "no preference" from "unset". `Option<Vec<RegionId>>` already
> expresses exactly that, matches how every other field in the shipped config is
> encoded, and removes a value that would otherwise need its own validation,
> display string, and migration rule.

### 7.3 Allowed values

A list of region IDs (e.g. `World`, `USA`, `Europe`, `Japan`) and language IDs
(e.g. `en`, `ja`, `de`), each non-empty, deduplicated, at most 16 entries.

### 7.4 Safe default

Empty (no preference) for both, at every scope.

### 7.5 Validation

Deduplicate on write; reject unknown IDs; reject a list longer than 16 entries.

### 7.6 Scope

Global by default; overridable per source and per platform.

### 7.7 Plain-language effect

*"When several versions of the same game are available, prefer them in this
order. Matching is never weakened to honour a preference."*

---

## 8. Revision policy

### 8.1 Definition

`RevisionPolicy` - how a source's *content version* is chosen. This generalises
today's cheat-source behaviour where the compiled-in source resolves the moving
`master` to an exact commit and pins the immutable archive to that commit.

### 8.2 Values

| Value | Meaning | Today's equivalent |
| --- | --- | --- |
| `FollowLatest` | Resolve the source's moving reference to the newest revision, pin it, and reuse it until a refresh is requested. | Cheat source today (`master` → exact SHA → immutable archive). |
| `Pinned` | Use exactly one pinned revision (commit ID for cheat sources; file digest / parsed-source fingerprint for DAT sources). | An archive pinned by `--expected-sha256`; a snapshot retained by pinning. |
| `Manual` | Never auto-resolve; the user explicitly supplies the revision to use. | `--offline` with an explicit snapshot; future DAT manual selection. |

### 8.3 Rules

- **`FollowLatest` is never `Latest` at use time.** The moving reference is
  resolved once, pinned, and that pinned revision is what everything downstream
  consumes. "Follow latest" describes when a *refresh* happens, not what is used
  during an operation.
- A `Pinned` revision that no longer verifies (digest mismatch) fails closed; it
  is reported, never silently replaced.
- The pin value is the immutable content identity: commit ID + archive SHA-256
  for cheat sources; for DAT sources the file bytes digest (the parsed-source
  fingerprint) once DAT persistence exists.
- A branch name alone is never a revision.

### 8.4 Safe default

`FollowLatest` for the compiled-in cheat source (identical to today).
DAT sources default to `Manual` until a DAT source registry is approved; the
first approved DAT source policy must pick its own default explicitly.

### 8.5 Validation

`Pinned` requires a well-formed pin value for the source type; `FollowLatest`
requires a resolvable reference; `Manual` requires a user-supplied revision on
use. Refresh cadence is runtime state, not policy (see §13).

### 8.6 Scope

Global by default; overridable per source and per platform.

### 8.7 Plain-language effect

*"Follow latest: use the newest revision when you ask to refresh. Pinned: only
ever use this exact revision. Manual: you choose the revision each time."*

---

## 9. Clone/parent policy

### 9.1 Definition

`ClonePolicy` - how DAT entries that declare a parent relationship
(`clone_of`, `sample_of`, `rebuild_to` in Logiqx; `cloneof`/`romof`/`rebuildto`
in ClrMamePro) are treated.

**This field lives in the DAT-specific policy block, not in the shared source
policy.** Cheat sources have no parent/clone relationships, so a shared
`clone_policy` would be inert on eight of the nine registered sources and would
need a "warned, not rejected" rule (§15.7) purely to excuse its own presence.

> REVIEW FIX — originally placed in shared policy "so the vocabulary is
> uniform". Uniform vocabulary is not worth a field that is meaningless for most
> sources; §9 (clone) and §13 (variant) are now explicitly per-kind. The shared
> layer keeps only identity, trust, enabled, priority, platform overrides, and
> provenance — the fields every source kind genuinely has.

### 9.2 Values

| Value | Meaning |
| --- | --- |
| `ExpandParents` | Materialise/consider cloned entries alongside their parent; audit reports the clone under its own name with the parent as provenance. |
| `PreferParent` | When a clone and its parent both match, report the parent; keep the clone as a secondary candidate. |
| `OnlyParents` | Ignore clone entries entirely; only parent (non-cloned) entries are audited. |
| `Standalone` | Ignore parent relationships; treat every entry as independent. |

### 9.3 Rules

- Clone/parent relationships are *evidence qualifiers*, never a replacement for
  hash evidence.
- A clone whose parent is absent is still auditable under `ExpandParents` and
  `Standalone`, but `PreferParent` and `OnlyParents` report it as
  "parent missing" rather than inventing a parent match.
- Cycles or self-references in the relationship graph are rejected at parse
  time and reported as warnings (parser concern; policy only decides use).

### 9.4 Safe default

`Standalone` - matches today's DAT audit, which does not consult parent
relationships when producing verdicts.

### 9.5 Plain-language effect

*"How ArchiveFS treats clone entries (a different region or revision of a game
that lists another entry as its parent)."*

---

## 10. Verified-only handling

### 10.1 Definition

`verified_only: bool` - whether the source is only permitted to contribute
*verified* (cryptographic-hash-backed) matches, or may also contribute probable
and filename-only evidence.

### 10.2 Rules

- `verified_only = true`: entries for which only CRC32/filename evidence exists
  are reported as `NoUsableEvidence` or `FilenameOnly` but are never promoted
  and never used for any action; they may still be listed as "not verified".
- `verified_only = false`: the full confidence hierarchy applies (Exact /
  Probable / FilenameOnly), matching today's DAT audit behaviour.
- This flag never upgrades a weak verdict; it only filters.
- The flag is orthogonal to trust level: an `Untrusted` source can still
  produce a hash-verified match that is *structurally* exact; trust governs
  provenance weight, verified-only governs the minimum evidence tier accepted.

### 10.3 Allowed values

`true` | `false`.

### 10.4 Safe default

`false` - today's audit reports probable and filename-only outcomes rather than
filtering them.

### 10.5 Scope

Global by default; overridable per source and per platform.

### 10.6 Plain-language effect

*"Only accept hash-verified matches from this source. Unverified name matches
are shown but never used."*

---

## 11. Duplicate/conflict policy

### 11.1 Definition

`ConflictPolicy` - how to present and resolve cases where two entries (from the
same or different sources) claim the same identity.

### 11.2 Values

| Value | Meaning | Today's equivalent |
| --- | --- | --- |
| `ReportAll` | Keep every candidate; report the conflict as a distinct outcome (`ExactMultipleCandidates`, `ProbableMultipleCandidates`, `Ambiguous`). | DAT audit collision handling today. |
| `PreferHighestPriority` | When the conflict is between sources, prefer the higher-priority source's claim, retaining the loser as a documented alternative. | No current equivalent (priority is new). |
| `BlockOnConflict` | A genuine conflict blocks the outcome entirely; no candidate is selected. | PCSX2/cheat matching conflict behaviour ("never install an uncertain match"). |

### 11.3 Rules

- **A conflict is never silently collapsed.** Whatever the policy, the
  alternatives remain visible in provenance.
- `ReportAll` is the only value that never makes a decision. `PreferHighestPriority`
  and `BlockOnConflict` make a decision but must record why.
- A 32-bit checksum collision is never treated as an exact match, regardless of
  policy (audit baseline §1.2).
- Conflict *within* the same source and platform where entries are byte-identical
  (same hashes, same name) is deduplicated as `Duplicate` before conflict policy
  applies; only genuine conflicting claims reach the conflict policy.

### 11.4 Safe default

`ReportAll` - today's behaviour for DAT audit; and the natural default for cheat
sources, whose matching already reports candidate sets.

### 11.5 Scope

Global by default; overridable per source and per platform.

### 11.6 Plain-language effect

*"When two entries claim the same game, show both instead of guessing (safe
default). Stronger options can prefer one source or refuse the match."*

---

## 12. Rename safety

### 12.1 Definition

`RenameSafety` - names the question of whether ArchiveFS may ever *suggest or
perform a rename* of a local file to match a source's canonical name (a
normalisation used by some DAT tooling).

The answer, for this design, is no. The type exists to make that answer explicit
and to give a future design somewhere to attach a different one.

### 12.2 Values

**DECISION (approved): `NeverSuggest` is the only implemented value. The other
two are future design only and must not appear as active GUI controls.**

| Value | Status | Meaning |
| --- | --- | --- |
| `NeverSuggest` | **Implemented; the only reachable value** | No rename is ever suggested or performed. |
| `SuggestPreviewOnly` | Future design only — not implemented, not selectable | A rename may be shown as a preview/plan, never applied. |
| `SuggestRequiresExplicitConfirm` | Future design only — not implemented, not selectable | A rename may be planned and applied only after per-file explicit confirmation. |

For the first implementation, `RenameSafety` is effectively a constant. It is
documented as an enum so the future design has a name to attach to, but a
conforming implementation may model it as a unit value and must reject any
attempt to persist another variant.

The two future values are retained in this document **only** so a later design
does not reinvent the vocabulary. They carry no schema commitment: if they are
ever implemented, that is a separate approved feature with its own migration.

### 12.3 Rules

- **ArchiveFS never renames a source archive.** The audit (§1, §7) and the
  project principle "never modify user archives" make `NeverSuggest` the only
  value until a separately approved feature allows otherwise.
- **No GUI control offers the other values.** Rename safety is rendered as a
  fixed statement of behaviour, not a selector — not even a disabled one, since
  a greyed-out "apply renames" option advertises a capability that does not
  exist (GUI §2.6).
- **No rename plan type ships in the first implementation**, so there is nothing
  for a review dialog to display (GUI §5.3).
- Should a future design implement either mode: rename targets are
  canonicalised, collision-checked, and must stay inside the source folder; a
  rename that escapes the folder is refused; renaming is never folded into an
  unrelated operation and requires its own confirmed plan; and renames never
  change identity, which is hash/path-based, not filename-based.

### 12.4 Safe default

`NeverSuggest` — and, for the first implementation, the only value.

### 12.5 Scope

Not scoped, because there is one value. A future design that implements the
other modes would decide their scope then; this document does not pre-approve
per-platform rename opt-in.

### 12.6 Plain-language effect

*"ArchiveFS never renames your files."*

No caveat, no "currently", and no mention of future modes. A user reading this
setting should come away certain, not wondering what might change.

---

## 13. Cheat variant policy

### 13.1 Definition

`VariantPolicy` - how a cheat source chooses between multiple *variants* of the
same cheat (for example "main game", "widescreen", "60 FPS", region-specific
variants) when a catalogue declares several.

### 13.2 Values

| Value | Meaning |
| --- | --- |
| `PreferCanonical` | Prefer the catalogue's canonical/first declared variant; others are retained as alternatives. |
| `RequireExplicitVariant` | Do not choose automatically; a variant must be selected explicitly. |
| `AllVariants` | Treat every variant as an independent candidate for matching/reporting. |

### 13.3 Rules

- Variant choice only matters *within* an already-established match; it never
  weakens the match.
- `PreferCanonical` matches today's RetroArch `.cht` treatment where the
  catalogue's declared set is presented and selection is explicit at install
  time (the audit's "fetch never installs; setup selects" boundary).
- A variant's `enabled_by_default` flag is metadata, not a policy decision.

### 13.4 Safe default

`PreferCanonical`, matching today's presentation order.

### 13.5 Scope

**Cheat-specific.** This field lives in the cheat-source policy block and does
not exist for DAT sources; it is overridable per source and per platform within
that block.

> REVIEW FIX — the original text both declared the field "irrelevant to DAT
> sources" and gave DAT sources a different default (`RequireExplicitVariant`)
> for it. A field cannot be both absent and defaulted. DATs do not carry
> variants, so the field is simply not part of DAT policy.

### 13.6 Plain-language effect

*"When a cheat exists in several forms (e.g. widescreen), prefer the standard
one unless you pick another."*

---

## 14. Provenance

### 14.1 Definition

Provenance is the immutable record of where a catalogue entry, snapshot, or
outcome came from and how it was verified. It is **not** policy; it is the
audit trail the policy layer reads and preserves.

### 14.2 What provenance records

- `SourceId`, `display_name` at acquisition time.
- The exact resolved revision (commit ID) and immutable archive URL/SHA-256 for
  cheat sources.
- The source file path and parsed-source fingerprint (bytes digest) for DAT
  sources, once DAT persistence exists.
- Retrieval timestamp, verification strength (`TransportOnly` today;
  `ChecksumPinned`/`SignatureVerified` as future strengths), and any warnings.
- For a given match outcome: which source, which priority, which effective
  policy fields applied, which evidence tier produced the verdict, and which
  alternatives were retained under the conflict policy.

### 14.3 Rules

- Provenance is append-only and never edited in place by the policy layer.
- Secrets (credentials, tokens) are never stored in provenance.
- Provenance survives source removal; removing a source does not delete its
  historical records.
- Provenance is shown to the user in the Effective Policy Summary and the
  Review Queue (GUI document).

### 14.4 Safe default

Provenance is always recorded; there is no opt-out.

### 14.5 Plain-language effect

*"ArchiveFS records where every catalogue entry came from and what evidence
backed each match, so you can see exactly why a result was reached."*

---

## 15. Effective-policy resolution

### 15.1 Scope precedence

For a lookup of `(source, platform)`:

1. `source.platform_overrides[platform]` (most specific)
2. `source` (source-level settings)
3. `platform_defaults[platform]` (platform-wide defaults)
4. `global_defaults` (least specific)

For a lookup without a platform (or with an unknown platform): steps 1 and 3 are
skipped.

### 15.2 Field-level merge

- Resolution is **field-by-field**, not whole-scope. A platform override that
  sets only `region_preferences` leaves every other field inherited from the
  source scope.
- Lists (`region_preferences`, `language_preferences`) replace, not append, at
  the more specific scope. There is no partial-list inheritance; a more specific
  scope that declares a preference list replaces the whole list.
- Booleans and enums are replaced outright at the more specific scope.

### 15.3 Priority aggregation within a space

Priority aggregation runs **within one space** (§5.2). The lookup key is
`(space, platform)`; DAT sources and cheat sources are never placed in the same
ordering, so there is no cross-space step.

When several sources *in the same space* can serve a game:

0. **Select the space** from the question being asked: identity/audit → DAT
   space; cheat content → cheat space.
1. **Sources that do not participate for the platform are excluded first**,
   before any ordering: source-level `enabled = false`, a platform
   `disabled_providers` entry, or a non-empty `spec.platforms` that does not
   list the platform. A source with an empty `platforms` list participates in
   every platform. For DAT sources this exclusion step is what makes priority
   platform-local (§5.2): only relevant DAT sources reach the comparator.
2. **Evidence tier dominates.** A verified (cryptographic-hash) match in a
   lower-ranked source beats a filename-only match in a higher-ranked source.
3. **Within the same evidence tier**, source priority decides — **lowest number
   first** — and ties break by `SourceId` lexicographic order. This is exactly
   the comparator `sorted_enabled` already implements.
4. **Platform override priority** participates in step 3: the effective priority
   for a platform is the source-level priority overridden by the platform
   override if present, clamped to `1..=999`.

If a single platform has only one relevant source in a space — the common case
for DAT sources — steps 3 and 4 have no observable effect at all.

### 15.4 Determinism

The resolution of any policy question is a pure function of the policy document
and the lookup key. It must produce identical results on repeated evaluation,
independent of map iteration order. A resolution function must not depend on
wall-clock time; time only affects `SourceRuntimeState`.

### 15.5 Resolution of a concrete example

| Field | global_defaults | source S | platform_defaults[PS2] | S.platform_overrides[PS2] | Effective for (S, PS2) |
| --- | --- | --- | --- | --- | --- |
| verified_only | false | false | false | true | **true** |
| region_preferences | — | [Europe] | — | — | **[Europe]** |
| clone_policy (DAT block only) | Standalone | Standalone | — | — | **Standalone** |
| priority | — | 20 | — | 5 | **5** (consulted earlier: lower wins) |

`—` means the field is absent at that scope and therefore inherits. Note the
priority override of `5` makes S *more* preferred than its source-level `20`,
because lower values are consulted first.

### 15.6 Conflict resolution between scopes

- A more specific scope never *invalidates* a non-overridable field; attempting
  to override identity, trust, or enabled state is a validation error (§6.3).
- Two more-specific rules that both claim to own the same field cannot exist in
  a well-formed document; the schema rejects duplicate keys for one field in one
  scope.

### 15.7 Validation of a resolved policy

A resolved policy must be self-consistent before it is used:

- `Pinned` revision policy requires a valid pin for the source type, and is only
  meaningful for a source whose `capabilities.download` is true.
- `verified_only = true` combined with a source that only ever produces
  filename evidence is *warned* (it will produce no matches), not rejected.
- A policy field set on a source kind that does not have it (a `clone_policy` on
  a cheat source, a `variant_policy` on a DAT source) is a **validation error**,
  not a warning — per-kind blocks (§9, §13) mean such a field cannot be reached
  by a well-formed document, so its presence indicates a corrupt or hand-edited
  file.

### 15.8 Where resolution happens

Resolution is a core-layer function, not a GUI or CLI responsibility. Both the
CLI and the GUI call the same resolver and display its output (GUI document,
"Effective Policy Summary"). This guarantees the audit's principle that "CLI and
GUI present the same plan and safety state from shared core types."

---

## 16. Runtime state (non-policy)

`SourceRuntimeState` records what a source *did*, and is explicitly out of the
user-authored policy document. **It is the shipped `CheatSourceHealth`**, which
already carries `state` (`CheatProviderSourceState`),
`last_checked_unix_seconds`, `last_error`, `entry_count`, and
`freshness_seconds`. The model adds:

- `last_successful_update` distinct from `last_checked` (a failed check must not
  look like a successful one),
- `last_accepted_version` (revision),
- `strongest_verification` observed,
- current cache/snapshot pointer (cheat sources with `capabilities.download`).

`health: None` means "not yet checked" and must never be rendered as a healthy
or a failed state. The registry never populates health itself; a caller sets it
after performing a real operation.

Runtime state is updated by the retrieval/audit pipelines and is read by the
GUI for status display. It never feeds back into trust, priority, or enabled
state without an explicit user action.

---

## 17. Relationship to existing types

- `CheatSourceSpec` / `CheatSourceEntry` (`cheat_source_registry`, PR #2) **are**
  the source model: `spec.id → SourceId`, `enabled → enabled`,
  `priority → priority`, `spec.platforms` → platform participation,
  `spec.capabilities` → what the source can do, `spec.upstream_project` /
  `spec.description` → provenance display. The model adds `trust_level` and
  `verification` to these types; it does not replace them.
- `CheatSourcesConfig` **is** the policy document (§1.1), extended.
- `CheatSourceHealth` **is** `SourceRuntimeState` (§16), extended. Note
  `health: None` means "not yet checked" and must stay distinguishable from a
  known-bad state.
- `CheatSourceDefinition` (`cheat_sources.rs`) stays the *retrieval* definition
  for the one downloadable RetroArch source:
  `provenance/licence_url/… → provenance fields`,
  `maximum_expected_bytes/… → resource policy` (kept in the shared
  resource-limits area per audit §6.1). It is not a second registry.
- `trust_status = "built_in_reviewed"` maps to `SourceTrustLevel::BuiltInReviewed`
  for the retrieval source; the other eight entries adopt the same value when the
  field is added (§3.2).
- `DatLimits`/`DatLimitsBuilder` remain subsystem-specific resource limits
  (audit §6.1) and are not folded into the source policy model; they are consumed
  by the same shared limit builder.
- `AuditVerdict` (DAT) and `MatchConfidence` (patch manager) remain distinct
  evidence types; the shared layer adds an explicit mapping note (audit §6.8) but
  does not merge them.

---

*Document created: 2026-08-04*
*Baseline: docs/design/DAT_CHEAT_POLICY_AUDIT.md (approved).*
*No production code is modified by this document.*
