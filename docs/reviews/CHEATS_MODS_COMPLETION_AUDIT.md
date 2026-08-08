# Current-main Cheats & Mods completion audit

Date: 2026-08-08. Branch: `feature/cheat-provider-finishing` (== `origin/main` @ `a2e5c56`).

This is a completion audit of the Cheats & Mods implementation as it actually
exists on current main. Every entry below names the exact module(s) that back
it. A module existing is not treated as the feature existing: "wired" means
reachable from the shipped GUI/CLI, exercised end-to-end, and covered by tests.

Classification key:

- **Complete + wired** - shipped, reachable, tested.
- **Implemented, not wired** - code exists but nothing reaches it.
- **Partial** - some of the workflow works, some does not.
- **Stub / no-op** - a marker with no behaviour.
- **Missing** - absent on main.
- **Historical branch only** - present only in an unmerged branch.
- **Deliberately unsupported** - documented as out of scope.

## 1. Cheat Sources GUI

**Complete + wired.** `crates/archivefs-gui/src/cheat_sources_page.rs` renders
every registered source with name, ID, emulator, provider kind, enabled state,
priority + consulted position, platform coverage, per-platform participation
toggles, platform-exception picker, and a save/revert bar with an honest
"save will …" consequence list (`show_source_row` :775, `show_save_bar` :708).
Unknown provider/platform preferences survive round-trip and are shown as
"Kept but not recognised" (`cheat_sources_page.rs:1067`).

## 2. Provider registry

**Complete + wired.** `cheat_source_registry/` (`mod.rs`, `config.rs`,
`capabilities.rs`) wires 9 entries / 6 upstream projects, with enable/disable,
priority, per-platform participation, and unknown-ID preservation
(`build_default_registry` :610). Re-exported from `patch_manager/mod.rs:172`.

## 3. Provider enable/disable state

**Complete + wired.** Persisted to `~/.config/archivefs/cheat_sources.toml`
(`config.rs:3`), edited by GUI page and `archivefs cheat-source
enable|disable` (`crates/archivefs-cli/src/cheat_source.rs`). Disabled sources
are still listed, and resolution skips them (`sorted_enabled`).

## 4. Provider priority

**Complete + wired.** `default_priority` per source; `set-priority` persists
and reorders; "consulted position" is 1-based over enabled sources
(`cheat_sources_page.rs:356-368`).

## 5. Platform participation

**Complete + wired.** `set_platform_participation` (`mod.rs:496`), overrides
persisted, source-level disable wins over platform blocks
(`PlatformParticipation` :54). GUI toggles per source.

## 6. Custom sources

**Not implemented (future-stage by design).** The registry documents "every
built-in provider and (in future stages) every user-configured custom source"
(`cheat_source_registry/mod.rs:4-5`). Unknown IDs in preferences are tolerated
and inert but never become usable sources. Historical branch
`feature/cheat-provider-custom-sources-stage1` predates main's registry and
has nothing to add (main's registry is a superset). **Deferred: needs a
provider-plugin model; not this PR.**

## 7. RetroArch provider

**Complete + wired.** `cheat_sources.rs` (online source, pinned-commit ZIP with
SHA-256 verification), `retroarch.rs` (advisory preview), `cheat_installer.rs`
+ `cheat_rollback.rs` (legacy apply/rollback, CLI), `retroarch_materialization.rs`
(shared-transaction apply in the GUI), cache lock + maintenance + entry-size
limits + streaming download. e2e `tests/retroarch_cheat_install_end_to_end.rs`.

## 8. Dolphin / GameCube provider

**Complete + wired.** `dolphin_gecko_provider.rs` (per-game Gecko/AR fetch),
`dolphin_cheat_catalogue.rs` (offline indexed catalogue, pinned commit),
`dolphin_local.rs` (profile discovery), `dolphin_gecko_install_plan.rs` +
`gecko_document.rs` (merge-preserving apply). e2e
`tests/dolphin_gecko_install_end_to_end.rs`.

## 9. PCSX2 provider

**Complete + wired.** `pcsx2.rs`, `pcsx2_identity.rs`, `pcsx2_local.rs`,
`pcsx2_provider.rs` (built-in metadata + strict compatibility gating),
`pcsx2_pnach.rs` (user content preserved byte-for-byte on merge),
`pcsx2_install_plan.rs`. e2e `tests/pcsx2_pnach_install_end_to_end.rs`.

## 10. Xenia / Xbox-family

**Complete + wired, honestly.** `xenia_provider.rs` (pinned-commit index +
per-file fetch, typed cache statuses), `xenia_local.rs`,
`xenia_patch_document.rs`, `xenia_install_plan.rs` (merge-preserving apply).
e2e `tests/xenia_patch_install_end_to_end.rs`.

## 11. GameHacking

**Complete + wired.** Real HTTP via `gamehacking_shared.rs` (Cloudflare
detection, rate-limit cooldown, robots.txt), PS2 (`gamehacking_provider.rs`),
GameCube (`gamehacking_gamecube_provider.rs`, sysID 13 fixture-backed), Wii
(`gamehacking_wii_provider.rs`, sysID 22). Apply via
`gamehacking_gamecube_install_plan.rs`. e2e install suites for GC and Wii.

## 12. CheatBase

**Missing on main.** Mentioned only as "possible future" in
`cheat_provider.rs:4`. Historical branch `feature/cheatbase-provider-stage1`
exists but is not merged. **Deferred: brand-new provider, no architecture
support; would be a separate PR.**

## 13. BSFree

**Complete + wired (read-only browse by design).** `bsfree.rs` downloads the
immutable pinned SQLite DB with SHA-256, has CLI (`archivefs cheats source
bsfree …`) and GUI operations, enable/disable persisted. Deliberately no
install API ("Stage 1 … no installation or conversion API", `bsfree.rs:4-5`).

## 14. Local / offline cheat catalogues

**Complete + wired.** `cheat_catalogue.rs` (offline .cht/manifest catalogue),
`cheat_candidates.rs` (ranked candidates from a local snapshot),
`retroarch_inventory.rs` (existing-library read-only). All offline.

## 15. Archive / ZIP ingestion

**Complete + wired.** Immutable snapshot downloads with archive-entry-count
limits and SHA-256 verification (`cheat_sources.rs`); catalogue validation
(`validate_entry_count`). No blind extraction.

## 16. Loose-ROM identity

**Complete + wired.** `game_identity.rs` records `LooseRomSha256`,
`LooseRomFormat`, `LooseRomTitle` evidence; loose-ROM re-verification at apply
(`main.rs:9312-9321` aborts before writing if the hash changed).

## 17. Multi-disc / game identity

**Complete + wired.** `game_identity.rs` evidence kinds cover Dolphin GameID /
revision / disc number / region, PS2 serial, PCSX2 CRC, XEX title/media ID.
Typed accessors (`verified_dolphin_game_id` :350, `verified_pcsx2_crc` :360,
etc.). Candidate matching is keyed on this identity, not filename guesses.

## 18. Preview

**Complete + wired.** `shared_preview.rs` is read-only (`build_with_hasher`
only opens files read-only; all writes are inside `#[cfg(test)]`). Every
write-capable provider stages bytes under a private staging root, never the
destination. "Preview never writes" is enforced and tested
(`staging_never_writes_outside_its_own_root`,
`preview_never_migrates_or_changes_the_catalogue_schema_version`).

## 19. Conflict detection

**Complete + wired.** Shared preview reports per-entry plan (InstallNew /
ReplaceExisting / RemoveInstalled), pre-digest, post-digest, existing state,
and overwrite/merge behaviour; same-name-different-body Gecko codes are an
explicit conflict ("ArchiveFS will not overwrite it", `gecko_document.rs:530`).

## 20. Apply

**Complete + wired.** `shared_transaction.rs`: atomic temp+fsync+rename
(`atomic_write` :1587), backup before replace (:1071, :1559), identity
re-check immediately before mutation (:904-991), plan/context/confirmation
binding (:640-646). Legacy RetroArch CLI path journals too
(`cheat_installer.rs:179`).

## 21. Rollback

**Complete + wired.** `execute_shared_rollback` (:1338) restores the exact
recorded backup, verifies restored digests, blocks a second rollback, reports
per-entry honesty; partial restore is never presented as complete
(:1455-1486). Legacy `execute_cheat_rollback_run` (`cheat_rollback.rs`) for
the CLI. GUI rollback via `start_shared_rollback`.

## 22. History & Logs

**Complete + wired.** `shared_transaction.rs` writes an operation journal
(`write_journal_once` :1683); `cheat_history.rs` discovers + inspects
journals, binds rollback availability, sorts newest-first. Journal entries
carry paths only inside the private history root; GUI history records carry
transaction id + counts, never private paths.

## 23. Doctor checks

**Partial - confirmed gap.** Doctor covers Emulators, EmulatorProfiles,
ManagedEntries ("ArchiveFS-managed cheat and patch entries") and Transactions,
but has no category probing cheat *source* health. `CheatSourceHealth`
(`cheat_source_registry/health.rs`) exists and `CheatSourceEntry.health` is a
field, but nothing populates it ("runtime health probing deferred",
`mod.rs:98`). **This PR populates health and surfaces it in the CLI and the
Cheat Sources page; wiring it into Doctor's category list is a documented
follow-up (PR #17+).**

## 24. First-run setup

**Partial (generic).** First-run "Welcome to ArchiveFS" banner and home card
for Cheats & Mods; the Dolphin/Xenia beginner route is the closest to
cheats-specific onboarding. No cheats-specific wizard.

## 25. Gamer View integration

**Complete + wired.** "Cheats & Mods" secondary action on the selected-game
panel, "← Back to games", "Undo last change" after an apply, home card, and
Library-row + Mount-page entry points.

## 26. Keyboard / compact-width GUI

**Partial (adequate).** Responsive collapse below 760px/300px
(`main.rs:29148`, :36784); default egui tab order; no custom arrow-key
navigation on the cheat workflow. Compact-width and keyboard tests exist
(`compact_platform_label_*`, `the_keyboard_mapping_requires_focus_on_the_shelf`).

## 27. Settings persistence

**Complete + wired.** `~/.config/archivefs/cheat_sources.toml`, only
non-default values persisted, unknown entries retained verbatim, save refuses
to overwrite an unparseable file. No custom-source path field (see item 6).

## 28. Unknown provider/platform round-trip

**Complete + wired.** Unknown `[[providers]]` and platform overrides are
preserved byte-for-byte and surfaced as unresolved
(`UnresolvedPreference` / `UnresolvedRowView`).

## 29. Cancellation / stale worker behaviour

**Complete + wired.** Fetch cancellation via `CheatSourceCancellation`;
apply-time staleness guarded by `CheatPreviewRequestKey` (archive, platform,
adapter, profile, source mode, source id, snapshot) compared at every
boundary (`main.rs:21881`, :9241, :10917); stale in-flight apply reset;
loose-ROM re-verification before write. Tested
(`stale_game_selection_is_rejected_before_preview`,
`stale_plan_source_and_destination_changes_fail_closed`,
`a_genuinely_stale_applying_transaction_is_still_reset`).

## 30. Network / provider error handling

**Complete + wired.** Typed error kinds per provider (CheatSourceError,
GameHackingErrorKind, GeckoProviderError, XeniaProviderFetchErrorKind,
BsFreeErrorKind), Cloudflare/rate-limit/stale-cache fallback handling, and
read-only cached fallback so a failed refresh never bricks discovery.

## Gaps implemented in this PR

1. **Cheat source health was defined but never populated.** `health.rs`'s
   `CheatSourceHealth` and `CheatSourceEntry.health` were inert: nothing
   computed them, the CLI's `info` health block could never print, and the
   GUI could not show a status. This PR adds a read-only probe per source over
   its persisted cache state (libretro `metadata.json`, BSFree `source.json` +
   validation, GameHacking catalogues, Dolphin catalogue, Gecko cache, Xenia
   index), wires it into the registry (`CheatSourceRegistry::probe_health`),
   the CLI (`cheat-source list`/`info`), and the Cheat Sources page (status
   badge, entry count, last checked, last error, and a "Refresh status"
   action). "PCSX2 official patch repository metadata" keeps no persisted
   state and honestly reads as not-checked.

## Gaps deliberately deferred (follow-up, PR #17+)

- **Doctor cheat-source category** (item 23): the probe is in place; wiring a
  new Doctor category + subsystem + gathered input is a separate, contained
  change.
- **Custom sources** (item 6): needs a provider-plugin model; the registry
  explicitly reserves it for a future stage.
- **CheatBase** (item 12): new provider, no architecture support; the
  `feature/cheatbase-provider-stage1` branch would need a proper review.
- **`game_presentation.rs`** is dead code (`SelectedGamePresentation` with
  `cheats_mods_available` is never referenced by production code).
- **`--resume`** on the three `gamehacking-*-index-refresh` CLI verbs is
  parsed and ignored (the crawl already resumes from cache by default).

## Provider coverage after this PR

| Provider | Online | Cache | Apply | Rollback | Health |
|---|---|---|---|---|---|
| RetroArch / libretro | yes | immutable snapshots + lock | shared + legacy | yes | probed |
| GameHacking PS2 | yes | catalogue.json | shared (PCSX2) | yes | probed |
| GameHacking GameCube | yes | catalogue.json | shared (Dolphin) | yes | probed |
| GameHacking Wii | yes | catalogue.json | shared (Dolphin) | yes | probed |
| Dolphin upstream Gecko | yes | per-game cache | shared | yes | probed |
| Dolphin catalogue | yes (one-time) | catalogue.json | n/a (preview) | - | probed |
| Xenia Canary | yes | index.json + files | shared | yes | probed |
| PCSX2 built-in | yes (fetch-time) | none persisted | shared | yes | not-checked |
| BSFree | yes (one-time) | SQLite + validation | n/a (read-only) | - | probed |
| CheatBase | - | - | - | - | missing (deferred) |
