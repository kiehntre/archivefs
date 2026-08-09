# DAT & Cheat-Source Policy GUI

Status: design proposal. This document is built on the approved baseline
[`docs/design/DAT_CHEAT_POLICY_AUDIT.md`](DAT_CHEAT_POLICY_AUDIT.md) and its
companion [`docs/design/DAT_CHEAT_POLICY_MODEL.md`](DAT_CHEAT_POLICY_MODEL.md).
It specifies the controls the GUI exposes for the policy model. No Rust code is
implemented here.

Scope: the GUI document defines the *policy surface*: DAT Sources, DAT Matching
Policy, Cheat Sources, File Safety, Review Queue, and Effective Policy Summary.
It does not redesign the existing catalogue manager, Cheats & Mods workspace, or
install transaction flows except where they must surface the new model. Where a
control already exists today, this document maps the existing behaviour onto the
new model so the migration (see the migration document) is explicit.

Grounding: control labels below follow the existing GUI's style as seen in
`crates/archivefs-gui/src/main.rs` (e.g. `show_retroarch_catalogue_manager`,
`catalogue_status_label`, `show_cheat_source_modes`, the "Review catalogue
download/update" window, "Confirm retrieval"). Every control listed here either
already exists (marked **existing**) or is proposed for the new model.

---

## 0. GUI principles inherited from the model

1. **Shared core types.** The GUI calls the same effective-policy resolver the
   CLI uses; it never re-implements policy logic. It renders what core returns.
2. **Safe defaults shown, not hidden.** Every control's safe default is shown
   inline, and the Effective Policy Summary always states the resolved value.
3. **No silent behaviour change.** Any control whose value differs from today's
   behaviour is surfaced with a "differs from current behaviour" marker until
   the user saves or reverts.
4. **Runtime state is displayed, never edited as policy.** Status like
   "Stale", "Update failed", and freshness come from `SourceRuntimeState` and
   are read-only in the policy screens.

For **every control** in this document the following attributes are given:

- **Label** — the on-screen text.
- **Purpose** — what the control decides.
- **Safe default** — the default value (matches today's behaviour unless noted).
- **Allowed values** — the value set.
- **Validation** — rules applied on save/edit.
- **Scope** — global, per-platform, or per-source; and whether it is overridable.
- **Plain-language effect** — the sentence shown to the user.

---

## 1. DAT Sources

Purpose of the section: register, enable, prioritise, and review local DAT
sources. Today DATs are CLI-only with no registry (audit §1.3, §3.1); this
section is new. It reuses the source-identity and trust vocabulary from the
model (§2, §3, §4, §5).

### 1.1 Add DAT source

- **Label:** "Add DAT source"
- **Purpose:** Register a local DAT file or DAT directory (Logiqx XML or
  ClrMamePro text) as a policy-bearing source so it can be prioritised,
  scoped, and audited alongside other sources.
- **Safe default:** n/a (action button).
- **Allowed values:** n/a; opens the *New DAT source* dialog (1.2–1.6).
- **Validation:** at most one "Add" action in flight; disabled while any
  source operation is running.
- **Scope:** global action (creates a source that is global by default).
- **Plain-language effect:** "Add a local DAT file or folder as a source you can
  enable, prioritise, and apply policy to."

### 1.2 Source ID (new DAT source dialog)

- **Label:** "Source ID"
- **Purpose:** The stable identity of the new source (model §2). Shown in
  provenance and audit output forever.
- **Safe default:** none — user-provided; validated as non-empty.
- **Allowed values:** ASCII letters, digits, `-`, `_`, `.`; 1–128 bytes; not
  `.` or `..`; no path separators; unique in the registry.
- **Validation:** reject empty, reserved, or duplicate IDs; reject characters
  outside the allowed set; reject leading/trailing `.`.
- **Scope:** source-level (identity is never per-platform).
- **Plain-language effect:** "The permanent name ArchiveFS uses to remember this
  source. It won't change if the file moves."

### 1.3 Source path

- **Label:** "Path"
- **Purpose:** Select the local DAT file or directory to register.
- **Safe default:** none; user chooses via the native file picker.
- **Allowed values:** an absolute, existing path to a regular file or directory.
- **Validation:** must be absolute; must not be a filesystem root; symlink
  components rejected (mirrors the project's existing source-folder and
  cache-root rules, audit §6.4); must exist at add time. `TrustedRoots`
  confinement applies to the path (audit §6.4 — the deliberate CLI exception for
  user-typed DAT paths does not extend to *configured* DAT paths).
- **Scope:** source-level.
- **Plain-language effect:** "The DAT file or folder this source reads."

### 1.4 Trust (new DAT source dialog)

- **Label:** "Trust level"
- **Purpose:** Record how the source's origin was reviewed (model §3).
- **Safe default:** `Untrusted` (model §3.4).
- **Allowed values:** `Untrusted` | `UserTrusted` (elevation requires an
  explicit confirm step that re-reads the origin, format, and path).
- **Validation:** `BuiltInReviewed` is not selectable for user-added sources;
  elevation requires a confirmation checkbox; changing scheme/origin/base path
  on an existing trusted source downgrades it and requires re-review.
- **Scope:** source-level (never per-platform).
- **Plain-language effect:** "Not reviewed yet — ArchiveFS's developers haven't
  checked this source. You can mark a source you have reviewed as 'You reviewed
  this'."

### 1.5 Enabled (new DAT source dialog)

- **Label:** "Enabled"
- **Purpose:** Whether this source participates in scans, audits, and matching
  (model §4).
- **Safe default:** `false` for a newly added, unverified DAT source (model
  §4.4); `true` only after explicit review or when the user knowingly enables it.
- **Allowed values:** `true` | `false`.
- **Validation:** none beyond the boolean; disabling never deletes runtime
  state or history.
- **Scope:** source-level.
- **Plain-language effect:** "Enabled sources are scanned and audited. Disabled
  sources are kept but not used."

### 1.6 Priority

- **Label:** "Priority"
- **Purpose:** Order this source relative to others when several can serve the
  same game (model §5).
- **Safe default:** `0`.
- **Allowed values:** integer in `1..=999`; **lower is consulted first**; ties
  broken by Source ID (model §5). Default `100` for a new DAT source.
- **Validation:** integer in range — an out-of-range value entered in the GUI is
  **rejected with a message**, not clamped, matching `cheat-source set-priority`.
  Ties need no resolution: the Source ID tie-break is total.
- **Scope:** the **DAT priority space only** (model §5.2). DAT sources are never
  ordered against cheat sources, and the two are shown as separate lists.
- **Presentation:** because DAT priority is only compared against other DAT
  sources relevant to the same platform, the control shows its *effect*, not
  just the number: for each platform the source covers, the resolved order of
  the relevant DAT sources. Where a platform has only one relevant DAT source —
  the common case — the GUI says so ("only source for this platform; priority
  has no effect here") rather than implying the number matters.
- **Plain-language effect:** "When two DAT sources cover the same platform, this
  decides which is consulted first — lowest number first. It has no effect on
  cheat sources, and none on platforms where this is your only DAT source. A
  real hash match always beats a name guess."

### 1.7 Platform overrides (per DAT source)

- **Label:** "Platform overrides"
- **Purpose:** Apply source policy to one platform only (model §6).
- **Safe default:** empty override map.
- **Allowed values:** a set of `(platform, field → value)` overrides for
  overridable fields only (priority, region/language preferences, revision
  policy, clone policy, verified-only, conflict policy, variant policy).
- **Validation:** platform IDs must come from the canonical registry; unknown
  IDs rejected; duplicate keys for one field in one scope rejected; overriding
  identity/trust/enabled state rejected.
- **Scope:** per-platform (overrides the source's global settings for that
  platform).
- **Plain-language effect:** "Settings that only apply to games on this
  platform. Everything else still uses the source's global settings."

### 1.8 Remove DAT source

- **Label:** "Remove source"
- **Purpose:** Deregister a DAT source.
- **Safe default:** n/a; requires explicit confirmation ("Remove source"
  typed/confirm button).
- **Allowed values:** n/a.
- **Validation:** requires confirmation; removal does not delete the underlying
  DAT file or historical provenance (model §14.3).
- **Scope:** source-level.
- **Plain-language effect:** "This source is no longer registered. The DAT file
  on disk and ArchiveFS's record of what it found are kept."

---

## 2. DAT Matching Policy

Purpose of the section: let the user see and set how DAT matches are treated —
evidence tiers, clones, region/language preference, verified-only filtering,
and conflicts — without touching parser internals. All of these map to model
fields and are resolved by the shared resolver.

### 2.1 Verified only

- **Label:** "Verified only"
- **Purpose:** Whether this source may contribute only hash-verified matches
  (model §10).
- **Safe default:** `false` (today's audit reports probable and filename-only
  outcomes rather than filtering them).
- **Allowed values:** `true` | `false`.
- **Validation:** `true` on a source that only ever produces filename evidence
  produces a *warning* ("this source will produce no matches"), not an error
  (model §15.7).
- **Scope:** global by default; overridable per source and per platform.
- **Plain-language effect:** "Only accept hash-verified matches from this
  source. Unverified name matches are shown but never used."

### 2.2 Clone / parent policy

- **Label:** "Clone handling"
- **Purpose:** How entries that declare a parent (`clone_of`/`cloneof`) are
  treated (model §9).
- **Safe default:** `Standalone` (today's audit ignores parent relationships
  when producing verdicts).
- **Allowed values:** `Standalone` | `ExpandParents` | `PreferParent` |
  `OnlyParents`.
- **Validation:** `PreferParent`/`OnlyParents` on a source type whose entries
  never carry parent metadata is warned, not rejected (model §15.7); parser-side
  relationship cycles are a parser warning, never a policy value.
- **Scope:** global by default; overridable per source and per platform.
- **Plain-language effect:** "How ArchiveFS treats clone entries — a different
  region or revision of a game that lists another entry as its parent. Default:
  treat every entry independently."

### 2.3 Region preference

- **Label:** "Preferred regions"
- **Purpose:** Ordered list used to disambiguate equal-tier matches (model §7).
- **Safe default:** empty (no preference).
- **Allowed values:** up to 16 region IDs from the canonical registry, e.g.
  `World`, `USA`, `Europe`, `Japan`; deduplicated on save.
- **Validation:** reject unknown IDs; deduplicate; cap at 16; order is
  meaningful and preserved.
- **Scope:** global by default; overridable per source and per platform.
- **Plain-language effect:** "When several versions of the same game are
  available, prefer them in this order. Matching is never weakened to honour a
  preference."

### 2.4 Language preference

- **Label:** "Preferred languages"
- **Purpose:** Ordered list used to disambiguate equal-tier matches by language
  (model §7).
- **Safe default:** empty (no preference).
- **Allowed values:** up to 16 language IDs (e.g. `en`, `ja`, `de`);
  deduplicated on save.
- **Validation:** reject unknown IDs; deduplicate; cap at 16; order preserved.
- **Scope:** global by default; overridable per source and per platform.
- **Plain-language effect:** "When several versions of the same game are
  available, prefer languages in this order."

### 2.5 Conflict policy

- **Label:** "When entries conflict"
- **Purpose:** How conflicting claims on the same identity are presented and
  resolved (model §11).
- **Safe default:** `ReportAll` (today's DAT collision reporting and cheat
  candidate reporting).
- **Allowed values:** `ReportAll` | `PreferHighestPriority` | `BlockOnConflict`.
- **Validation:** `PreferHighestPriority` and `BlockOnConflict` must record a
  reason when they make a decision; a 32-bit checksum collision is never exact
  under any value.
- **Scope:** global by default; overridable per source and per platform.
- **Plain-language effect:** "When two entries claim the same game, show both
  instead of guessing. Stronger options can prefer one source or refuse the
  match."

### 2.6 Rename safety

- **Label:** "File renaming"
- **Purpose:** Tell the user, unambiguously, that ArchiveFS does not rename
  their files (model §12).
- **Safe default:** n/a — **this is not a control.** It is a fixed statement of
  behaviour rendered in the policy summary.
- **Allowed values:** none. `NeverSuggest` is the only implemented value
  (model §12.2), so there is nothing to select.
- **Validation:** n/a. A policy document that somehow carries another
  `RenameSafety` value is rejected at load, not honoured.
- **Scope:** all.
- **Plain-language effect:** "Your files won't be renamed unless you approve it."

> DECISION (approved) — rename safety is **not** exposed as a selector, and not
> as a disabled or greyed-out one either. A visible "apply renames" option, even
> inactive, advertises a capability that does not exist and invites users to ask
> for it to be turned on. The two future modes live in the model document as
> vocabulary for a later design; they have no GUI presence of any kind until
> that design is separately approved.

### 2.7 Run audit

- **Label:** "Run audit"
- **Purpose:** Execute the DAT audit with the currently resolved policy.
- **Safe default:** n/a (action button).
- **Allowed values:** n/a.
- **Validation:** at least one enabled DAT source required; results rendered
  through the shared core `audit` types.
- **Scope:** applies to the selected source/scope context.
- **Plain-language effect:** "Check your files against this DAT using the
  current policy. Nothing is modified."

---

## 3. Cheat Sources

Purpose of the section: the existing RetroArch cheat catalogue manager
(audit §2.4; GUI `show_retroarch_catalogue_manager`), extended so its displayed
state and actions map cleanly onto the model. Existing controls are listed first,
followed by model-aligned additions.

**Priority context: this section closes a shipped user-control gap.** The nine
registry entries (audit §2.2) and their enabled/priority preferences exist today
but are reachable **only** from `archivefs cheat-source list | info | enable |
disable | set-priority`. No GUI screen lists them. Per-platform overrides are
worse: they are readable by the code but writable only by hand-editing
`~/.config/archivefs/cheat_sources.toml`.

Sections 3.6 and the new 3.10–3.11 below are therefore not speculative design;
they surface behaviour that already ships.

**DECISION (approved): these are the first implementation milestone.** Milestone
1 is exactly four capabilities over the existing nine cheat sources:

1. **List** them (3.10).
2. **Enable / disable** each one (3.6).
3. **Set priority** (3.11).
4. **Per-platform participation** (3.11).

**Milestone 1 adds no new persistence format fields.** It reads and writes
`cheat_sources.toml` in exactly its shipped shape — `providers[]` and
`platform_overrides[]`, no `format_version`, no trust field, no new keys. Every
model field in §7–§13 and every DAT control in §1–§2 is out of scope for it.

This keeps milestone 1 fully reversible: reverting the GUI leaves a file byte-
compatible with what every released binary already reads. It also delivers the
largest user-control gain in the design — the nine sources becoming visible and
controllable — before any schema risk is taken on.

Not in milestone 1: trust display (3.5, needs a core field), revision policy
(3.7), expected digest (3.8), download limits (§4.5), and everything in §1, §2,
§5.2–§5.4 and §6.

### 3.1 Catalogue status

- **Label:** status text: "Missing", "Ready", "Verified with warnings",
  "Stale", "Invalid manifest", "Incomplete", "Unsupported schema",
  "Verification failed", "Retrieval failed", "Cancelled", "Resource limit
  reached" (existing `catalogue_status_label`).
- **Purpose:** Show the source's runtime state from `SourceRuntimeState`
  (model §16), not policy.
- **Safe default:** n/a (derived from state).
- **Allowed values:** the listed status set.
- **Validation:** n/a (read-only display).
- **Scope:** per-source.
- **Plain-language effect:** "This is the current state of this catalogue, not a
  setting: what is cached, how fresh it is, and whether it can be used."

### 3.2 Download

- **Label:** "Download"
- **Purpose:** Fetch the catalogue for the first time (no snapshot present).
- **Safe default:** hidden when a snapshot already exists (only "Update" shows).
- **Allowed values:** n/a (action button, enabled when status is `Missing`).
- **Validation:** disabled while a retrieval is running or a review dialog is
  open; opens the Review Queue confirmation (§5.1) before any network access.
- **Scope:** per-source.
- **Plain-language effect:** "Fetch the trusted cheat catalogue. Nothing is
  installed — apply is always a separate, confirmed step."

### 3.3 Update

- **Label:** "Update"
- **Purpose:** Refresh an existing snapshot to a newer revision.
- **Safe default:** shown only when a snapshot already exists; requires the same
  Review-then-Confirm path as Download.
- **Allowed values:** n/a (action button).
- **Validation:** disabled while running/reviewing; the active snapshot remains
  active until the new one verifies (audit §2.2).
- **Scope:** per-source.
- **Plain-language effect:** "Refresh to the newest revision. If the refresh
  fails, your existing catalogue stays active and usable."

### 3.4 Verify

- **Label:** "Verify"
- **Purpose:** Re-verify the cached snapshot's manifest and per-file digests,
  locally, with no network access.
- **Safe default:** always available (read-only).
- **Allowed values:** n/a (action button).
- **Validation:** disabled while running; performs only local digest checks.
- **Scope:** per-source.
- **Plain-language effect:** "Re-check the files already on disk against the
  recorded digests. Uses no network."

### 3.5 Trust classification

- **Label:** "Trust classification" (existing, shown in technical details)
- **Purpose:** Display the source trust level mapped from the model (§3).
- **Safe default:** `built_in_reviewed` for all nine built-in registry entries.
- **Allowed values:** display of `BuiltInReviewed` | `UserTrusted` | `Untrusted`.
- **Validation:** read-only display; changes only through the explicit review
  flow. **Blocked on core work:** `CheatSourceSpec` has no trust field today
  (audit §2.2), so this control cannot render for the eight non-retrieval
  sources until model §3 is implemented. Until then the GUI must omit the field
  rather than defaulting it to "Reviewed", which would assert a review that has
  not happened.
- **Wording is mandatory, not cosmetic.** Per model §3.1, trust describes the
  *integration only*. The label must never render as a bare "Reviewed",
  "Trusted", or a check-mark badge. Required renderings:

  | Value | Rendering |
  | --- | --- |
  | `BuiltInReviewed` | "Built-in integration — upstream content not reviewed" |
  | `UserTrusted` | "You reviewed this source's setup" |
  | `Untrusted` | "Not reviewed yet" |

  Wherever a source's content is about to be previewed or applied, the
  accompanying text states plainly that codes come from the upstream community
  and ArchiveFS does not endorse them.

> DECISION (approved) — shipping a scraper or provider does not mean ArchiveFS
> reviewed or endorsed the upstream content. Six of the nine built-in entries
> carry community-submitted codes no maintainer has read. A bare "Reviewed"
> badge on those rows would be a false claim, so the scope travels with the
> label everywhere it appears.
- **Scope:** per-source.
- **Plain-language effect:** "Reviewed by ArchiveFS's developers", "You reviewed
  this", or "Not reviewed yet."

### 3.6 Enabled (cheat source)

- **Label:** "Enabled"
- **Purpose:** Whether the cheat source participates in retrieval and setup
  (model §4).
- **Safe default:** `true` for **all nine** registry entries (matches
  `CheatSourceEntry::from_spec`).
- **Allowed values:** `true` | `false`.
- **Validation:** boolean; disabling keeps cache and history. The toggle writes
  `~/.config/archivefs/cheat_sources.toml` — the GUI must name that file in the
  confirmation, as the CLI already prints it.
- **Scope:** source-level. Distinct from per-platform participation (3.11).
- **Plain-language effect:** "Enabled sources can be downloaded and used.
  Disabled sources are kept but not used."
- **Existing equivalent:** `archivefs cheat-source enable|disable <id>`.

### 3.7 Revision policy (cheat source)

- **Label:** "Revision policy"
- **Purpose:** How the source's content version is chosen (model §8).
- **Safe default:** `FollowLatest` (today's behaviour: `master` is resolved to
  an exact commit and pinned).
- **Allowed values:** `FollowLatest` | `Pinned` | `Manual`.
- **Validation:** `Pinned` requires a valid pin (commit + archive SHA-256);
  `Manual` requires a revision supplied on use; branch names alone are never a
  revision.
- **Scope:** global by default; overridable per source and per platform.
- **Plain-language effect:** "Follow latest: use the newest revision when you
  ask to refresh. Pinned: only this exact revision. Manual: you choose each
  time."

### 3.8 Expected digest

- **Label:** "Expected SHA-256" (existing `--expected-sha256` CLI; GUI exposure
  is proposed)
- **Purpose:** Optional independently-obtained expected archive digest used as
  additional verification (model §8, verification config).
- **Safe default:** none (unset).
- **Allowed values:** 64 lowercase hex characters.
- **Validation:** must be exactly 64 hex chars; a mismatch fails closed.
- **Scope:** per-source.
- **Plain-language effect:** "If you have an independently published digest for
  this archive, ArchiveFS will require the download to match it."

### 3.9 Source mode cards (Cheats & Mods)

- **Label:** "Existing RetroArch library" and "ArchiveFS trusted catalogue"
  (existing `show_cheat_source_modes`), plus the planned
  "Local unverified source" and "Remote unverified source" cards.
- **Purpose:** Choose the source mode before matching/install (the model's
  trust + revision concepts applied at workflow level).
- **Safe default:** the current radio selection; no default auto-selection
  change on migration.
- **Allowed values:** the four cards; switching clears dependent candidate state
  (existing behaviour).
- **Validation:** local-unverified and remote-unverified remain future
  workflows ("A real bounded local inspection backend is required…");
  "User-defined remote sources are a future workflow." — the cards are rendered
  as planned/pending.
- **Scope:** workflow-level (per archive being worked on).
- **Plain-language effect:** "Existing RetroArch library: read-only inventory of
  your installed cheats. ArchiveFS trusted catalogue: a validated cached
  snapshot. Unverified sources are future options, shown but not active."

### 3.10 Cheat source list

- **Label:** "Cheat sources"
- **Purpose:** List all nine registry entries in effective order, with their
  enabled state, priority, emulator, platforms, capabilities, and health. This
  is the GUI equivalent of `cheat-source list`.
- **Safe default:** n/a (display + row controls).
- **Allowed values:** read-only columns plus the 3.6 toggle and 3.11 priority
  editor per row.
- **Validation:** disabled sources remain listed **in the same position**, never
  hidden or moved — `sorted_all` guarantees this, and the GUI must not re-sort
  independently. Health of `None` renders as "Not checked", never as healthy or
  failed.
- **Scope:** global list.
- **Plain-language effect:** "Every cheat source ArchiveFS knows about, in the
  order it consults them. Disabled ones stay in place so you can see what you
  turned off."

### 3.11 Priority and per-platform participation

- **Label:** "Priority" and "Platform overrides"
- **Purpose:** Reorder a source globally, or stop it contributing for one
  platform without disabling it everywhere (model §5, §6).
- **Safe default:** the source's `default_priority` (model §5.4); no platform
  overrides.
- **Allowed values:** priority `1..=999`; per platform, a participation toggle
  and an optional priority override.
- **Validation:** out-of-range priority rejected, not clamped; platform names
  resolved through `canonical_platform_for_alias` with an unresolvable name
  reported inline rather than silently ignored; a saved file must round-trip
  entries the GUI does not recognise rather than dropping them (model §6.3).
  **The round-trip fix is a prerequisite** — see §3.12.
- **Scope:** the **cheat priority space only** (model §5.2); source-level and
  per-platform within it. Never merged with DAT ordering.
- **Presentation:** the ordering is shown as positions ("consulted 1st, 2nd,
  3rd…"), with the raw number secondary. Lower-wins is preserved for
  compatibility (model §5.1) but reads backwards, so the resolved order is what
  the user sees and reorder controls act on position, not on arithmetic.
- **Plain-language effect:** "Change where this source sits in the order, or
  leave it on but stop using it for one platform."
- **Existing equivalent:** `cheat-source set-priority`; per-platform overrides
  have **no CLI equivalent** and are currently hand-edited TOML only.

### 3.12 Preference-file integrity (prerequisite)

- **Label:** none — a correctness requirement on every write in 3.6 and 3.11.
- **Purpose:** Ensure editing preferences in the GUI cannot destroy parts of the
  file the GUI does not understand.
- **Required behaviour:**
  - A `providers[]` entry whose ID matches no registry source is **preserved
    verbatim** on save, not dropped. Today `to_config()` re-serialises only live
    registry entries, so such a line disappears on the next write (audit §3.2).
  - A `platform_overrides[]` entry whose platform name does not canonicalise is
    **preserved verbatim** and surfaced to the user as unresolved, not silently
    ignored.
  - Unresolved entries are shown in the GUI as "not recognised — kept as
    written", with the offending ID or platform name, so the user can correct a
    typo instead of losing the line.
  - Writes are durable (migration §3.3): file `sync_all` and parent-directory
    sync before the rename is considered complete.
- **Validation:** a round-trip test — load a file containing an unknown provider
  ID and an unresolvable platform name, toggle an unrelated source, save, and
  assert both unknown entries survive byte-for-byte.
- **Scope:** every preference write.
- **Plain-language effect:** "Changing one setting never deletes another. If
  ArchiveFS doesn't recognise something in your preferences file, it keeps it
  and tells you."

> DECISION (approved) — this is a **prerequisite bug fix**, required before any
> schema migration. Opening the schema while writes still drop unrecognised
> entries would mean the migration itself could destroy user data, and a
> half-migrated file is exactly the case where unknown keys are most likely to
> be present.

---

## 4. File Safety

Purpose of the section: surface the concrete structural-safety outcomes of the
trust/verification model (audit §2.2; `docs/CHEATS_MODS_USER_POLICY.md`). These
are read-only displays and confirmation gates; they do not add policy knobs
beyond what the model defines.

### 4.1 Exclusion examples

- **Label:** "Bounded exclusion examples" (existing)
- **Purpose:** Show representative entries excluded from a verified snapshot and
  why (typed exclusion: malformed `.cht`, unsupported content/encoding, unsafe
  path).
- **Safe default:** at most 32 representative examples retained (existing
  constant).
- **Allowed values:** list of `(kind, relative path)` pairs.
- **Validation:** read-only; paths never reconstructed for non-UTF-8
  (lossy-marked display only).
- **Scope:** per-source snapshot.
- **Plain-language effect:** "These files were kept but not indexed, because of
  the reason shown. The snapshot is still structurally verified."

### 4.2 Snapshot SHA-256

- **Label:** "Snapshot SHA-256" (existing)
- **Purpose:** Show the immutable content digest that pins the active snapshot
  (model §8 provenance).
- **Safe default:** derived from the active snapshot.
- **Allowed values:** 64 hex chars (display).
- **Validation:** read-only.
- **Scope:** per-source.
- **Plain-language effect:** "The exact fingerprint of the catalogue currently
  in use. Nothing else can stand in for it."

### 4.3 Verification outcome banner

- **Label:** "Verified", "Verified with warnings", "Verification failed",
  "Update failed · existing catalogue remains active and usable" (existing
  banners)
- **Purpose:** State clearly whether a snapshot is complete, partial, or failed,
  and that a failure never replaces the last known-good snapshot.
- **Safe default:** n/a (derived).
- **Allowed values:** the status/banner set.
- **Validation:** read-only; tone follows severity (Success/Warning/Blocked).
- **Scope:** per-source.
- **Plain-language effect:** "What verification found. A failed update never
  removes your existing usable catalogue."

### 4.4 Unsafe-path refusal (confirmation gate)

- **Label:** (no user-facing label; a gate) — refusal text per existing banner
  style: "Destination unavailable", "Unsafe snapshot path", etc.
- **Purpose:** Block actions whose inputs are structurally unsafe (symlink,
  traversal, special file, digest mismatch) before any write.
- **Safe default:** always on; cannot be disabled by policy.
- **Allowed values:** n/a (hard gate).
- **Validation:** core-layer path/digest checks (audit §6.4, §6.6); never
  overridable from the GUI.
- **Scope:** all.
- **Plain-language effect:** "This is a concrete technical hazard, not a matter
  of choice, so it is always refused."

### 4.5 Download limits

- **Label:** "Download limit" (proposed; mirrors `--max-download-bytes`)
- **Purpose:** Bound the maximum accepted download bytes for a source.
- **Safe default:** `CheatSourceDefinition.maximum_expected_bytes` (256 MiB
  today); lowering is allowed, raising above that bound is refused.
- **Allowed values:** integer bytes in `[1, maximum_expected_bytes]`.
- **Validation:** must be greater than zero; must not exceed the ceiling;
  enforced during streaming.
- **Scope:** **only sources with `capabilities.download`.** This is a field of
  `CheatSourceDefinition` (the retrieval definition), not of `CheatSourceSpec`,
  so today it exists for `libretro-buildbot-cheats` alone. The control must be
  hidden, not shown-and-disabled, for the other registry entries — rendering a
  byte ceiling for a source that never downloads implies a bound that is not
  enforced anywhere (resource policy, audit §6.1, §6.5).
- **Plain-language effect:** "ArchiveFS will stop a download that exceeds this
  size."

---

## 5. Review Queue

Purpose of the section: the existing Review-then-Confirm path (audit §2.4;
`CatalogueManagerAction::Review/Confirm/CancelReview`), generalised so it also
handles policy-elevating and policy-changing actions, and enriched with the
provenance the model requires (§14).

### 5.1 Review catalogue download / update

- **Label:** "Review catalogue download" / "Review catalogue update" (existing
  window title)
- **Purpose:** Confirm, per action, that the user is authorising network access
  for this exact request.
- **Safe default:** action does not start until confirmed; closing the window
  cancels.
- **Allowed values:** "Confirm retrieval" | "Cancel".
- **Validation:** review window content states: provider, managed destination,
  revision resolution + immutable HTTPS download + verification description;
  network begins only after confirmation.
- **Scope:** per-source, per-action.
- **Plain-language effect:** "Network access begins only after you confirm this
  exact request. Nothing is installed by fetching."

### 5.2 Review trust elevation

- **Label:** "Review this source" (proposed)
- **Purpose:** Elevate a source from `Untrusted` to `UserTrusted` after showing
  its origin, format, and path.
- **Safe default:** `Untrusted`; elevation requires this review.
- **Allowed values:** "Mark as reviewed" | "Cancel".
- **Validation:** the review shows the exact source ID, path/URL, host, and
  limits; elevation is recorded in provenance; a material change later
  downgrades and requires re-review (model §3.3).
- **Scope:** per-source.
- **Plain-language effect:** "You are telling ArchiveFS you have reviewed this
  source and want to trust it. This only changes how ArchiveFS labels it — it
  does not disable any safety check."

### 5.3 Review rename plan — not implemented

**This dialog does not exist and must not be built.** Rename safety is fixed at
`NeverSuggest` (model §12.2), so no rename plan is ever produced and there is
nothing to review. The entry is retained here only to record that its absence is
deliberate.

If a future approved design implements `SuggestPreviewOnly` or
`SuggestRequiresExplicitConfirm`, this dialog is specified then, together with
the plan type it displays. Nothing in the first implementation should include a
placeholder for it.

### 5.4 Review conflict resolution (future)

- **Label:** "Resolve conflict" (proposed, shown only under
  `PreferHighestPriority`/`BlockOnConflict`)
- **Purpose:** Show both sides of a genuine identity conflict before any
  decision is applied.
- **Safe default:** `ReportAll`; this dialog does not appear at default
  settings.
- **Allowed values:** "Keep both" | "Use preferred source" | "Block".
- **Validation:** both candidates and their provenance shown; the choice is
  recorded; never presented as a single "best" answer.
- **Scope:** per-conflict.
- **Plain-language effect:** "Two entries claim the same game. See both and
  decide, rather than ArchiveFS guessing."

---

## 6. Effective Policy Summary

Purpose of the section: a single, always-visible panel that shows the resolved
policy for the current context (model §15). It is the GUI's guarantee of the
audit principle that CLI and GUI present the same safety state from shared core
types.

### 6.1 Resolved values panel

- **Label:** "Effective policy"
- **Purpose:** Display the fully resolved policy for the currently selected
  source/platform: verified-only, clone handling, region/language preference,
  revision policy, conflict policy, rename safety, variant policy, and priority.
- **Safe default:** shows the resolved value even when unset by the user (the
  safe default is what appears).
- **Allowed values:** read-only render of the resolver output.
- **Validation:** n/a (display); the panel is regenerated from the resolver on
  any policy edit or scope change.
- **Scope:** reflects current source+platform context; a global view shows
  global resolution.
- **Plain-language effect:** "This is exactly what ArchiveFS will do with the
  settings above, after applying platform overrides."

### 6.2 Where a value comes from

- **Label:** "Source of value"
- **Purpose:** Show which scope supplied each resolved field (global, source,
  platform default, or platform override), so override behaviour is legible.
- **Safe default:** resolved value + source-of-value both shown.
- **Allowed values:** read-only tags per field.
- **Validation:** n/a (display).
- **Scope:** per-field.
- **Plain-language effect:** "For each setting, where it came from — global,
  this source, or this platform only."

### 6.3 Differs-from-default marker

- **Label:** "Differs from current behaviour"
- **Purpose:** Flag any resolved value that is not the safe default, so a user
  never mistakes a changed setting for the default (GUI principle §0.3).
- **Safe default:** marker absent when resolved value equals the safe default.
- **Allowed values:** present/absent per field.
- **Validation:** derived from the resolver's safe-default table.
- **Scope:** per-field.
- **Plain-language effect:** "This setting is not ArchiveFS's default. You can
  revert it with one click."

### 6.4 Revert to default

- **Label:** "Reset all to safe defaults"
- **Purpose:** Return every policy field in the current scope to its safe
  default.
- **Safe default:** n/a (action).
- **Allowed values:** "Reset" | "Cancel".
- **Validation:** requires confirmation; records the change in provenance;
  resolves to the same defaults the migration document guarantees.
- **Scope:** current scope (global or the selected source/platform override).
- **Plain-language effect:** "Put every setting in this scope back to
  ArchiveFS's safe defaults."

### 6.5 Runtime status strip

- **Label:** status strip (existing pattern: trust state, freshness, last
  update/failure)
- **Purpose:** Separate what the source *is* (policy) from what it *did*
  (runtime state) at a glance.
- **Safe default:** n/a (derived).
- **Allowed values:** read-only badges (e.g. "Trusted", "Not reviewed",
  "Stale", "Last successful update: …").
- **Validation:** n/a (display).
- **Scope:** per-source.
- **Plain-language effect:** "Top line: how this source is classified. Below:
  its current status and history. They are kept separate on purpose."

---

## 7. Summary of new vs existing controls

| Area | Existing today | New in this design |
| --- | --- | --- |
| DAT Sources | none (no registry at all; `dat` CLI takes a path per invocation) | Add/ID/path/trust/enabled/priority, platform overrides, remove |
| DAT Matching Policy | audit behaviour in CLI output | verified-only, clone policy, region/language, conflict, rename safety |
| Cheat source list | **CLI-only** (`cheat-source list`/`info`) | GUI list of all nine entries (3.10) |
| Cheat source enabled / priority | **CLI-only** (`cheat-source enable`/`disable`/`set-priority`), persisted to `cheat_sources.toml` | GUI toggle + priority editor (3.6, 3.11) |
| Per-platform participation / priority | **hand-edited TOML only** — no CLI, no GUI | GUI platform-override editor (3.11) |
| Cheat source trust level | **does not exist** in the registry | new `trust_level` field (model §3), then GUI display (3.5) |
| Cheat retrieval (RetroArch) | status, Download/Update/Verify, trust classification, source-mode cards, expected digest (CLI) | revision policy as an explicit model field; source-mode cards mapped to model |
| File Safety | exclusion examples, SHA-256, banners, hard gates | download-limit control surfaced in GUI, for downloading sources only |
| Review Queue | catalogue download/update review, Confirm retrieval | trust-elevation review, rename review, conflict review |
| Effective Policy Summary | none | resolved values, source-of-value, differs-from-default, reset, runtime strip |

The three **CLI-only / TOML-only** rows are the user-control gaps that already
exist in shipped code. They are the highest-value and lowest-risk part of this
design, and they depend on no new model field.

---

*Document created: 2026-08-04*
*Baseline: docs/design/DAT_CHEAT_POLICY_AUDIT.md (approved); model:
docs/design/DAT_CHEAT_POLICY_MODEL.md.*
*No production code is modified by this document.*
