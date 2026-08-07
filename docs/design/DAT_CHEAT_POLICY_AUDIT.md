# DAT & Cheat-Source Policy Audit

This document is a design-time audit of the current DAT catalogue and cheat-source
subsystems in ArchiveFS. It is intended to identify where the two systems already
share concerns, where each has its own policy, and where a future shared policy
layer could be introduced without disturbing existing behaviour.

Scope: `crates/archivefs-core`, `crates/archivefs-cli`, `crates/archivefs-gui`.
No code is implemented here; the document is input for a later design approval.

**Baseline: `main` at `77f69cb` (includes PR #2,
`review/cheat-sources-stage1-fixes`).**

> REVIEW FIX — this audit was originally written against `d0d2334`, which
> predates PR #2. PR #2 merged a nine-entry `cheat_source_registry`, a persisted
> per-user preferences file, and a `cheat-source` CLI. Sections 2, 3.2 and 6.2
> have been rewritten against the merged tree; every statement of the form "there
> is only one source" or "there are no config keys" was invalidated by that merge
> and is corrected below. The DAT subsystem is unchanged by PR #2, so sections 1,
> 3.1 and 4.1 stand as written.

---

## 1. Current DAT architecture

### 1.1 Module layout

The DAT subsystem lives in `archivefs-core::dat` and is deliberately read-only:

- `crates/archivefs-core/src/dat/mod.rs` — public module exports: `audit`,
  `hash`, `index`, `limits`, `model`, `parser`, `parsers`, and a `regression`
  test module.
- `crates/archivefs-core/src/dat/model.rs` — provider-neutral shapes:
  `ParsedDat`, `DatSource`, `DatGameEntry`, `DatRomEntry`, `DatChecksum`,
  `DatEcosystem`, `DatFormat`, and `ChecksumAlgorithm` (CRC32, MD5, SHA-1,
  SHA-256).
- `crates/archivefs-core/src/dat/parsers/mod.rs` — format sniffing and dispatch
  between Logiqx XML and ClrMamePro text.
- `crates/archivefs-core/src/dat/parsers/logiqx.rs` — streaming XML parser for
  No-Intro/Redump-style DATs.
- `crates/archivefs-core/src/dat/parsers/clrmamepro.rs` — line-oriented parser
  for TOSEC-style DATs.
- `crates/archivefs-core/src/dat/index.rs` — in-memory collision-aware indexes
  keyed by CRC32, MD5, SHA-1, SHA-256, and filename.
- `crates/archivefs-core/src/dat/audit.rs` — read-only audit that compares
  caller-supplied hashes against a `DatIndex`.
- `crates/archivefs-core/src/dat/limits.rs` — configurable resource ceilings
  (`DatLimits`, `DatLimitsBuilder`).
- `crates/archivefs-core/src/dat/hash.rs` — lowercase-hex normalisation for the
  four checksum algorithms.
- `crates/archivefs-core/src/dat/regression.rs` — regression tests for the
  `feature/dat-audit-stage1` fixes.

### 1.2 Key design decisions

- **Provider-neutral model.** `ParsedDat` and `DatIndex` are the same regardless
  of whether the input came from Logiqx XML or ClrMamePro text.
- **Streaming XML, bounded memory.** `parse_logiqx` uses `quick-xml` pull events;
  the *document* is streamed, but the parsed `Vec<DatGameEntry>` is held in memory.
  `limits.rs` documents the real memory multiplier (~8–9× file size at the time of
  writing).
- **Security: XML entities are inert.** `quick-xml` is used with
  `default-features = false`, so DOCTYPE declarations are accepted as inert text,
  external entities are never fetched, and declared entities are never expanded.
  `billion_laughs_is_neutralised` and `external_entity_reference_reads_no_file`
  lock this down.
- **Malformed checksums are dropped with warnings.** `DatChecksum::parse`
  returns `None` for values that are not the expected length of lowercase hex.
- **Collision-aware indexing.** `DatIndex` keeps every ROM reference for every
  hash, so `ExactMultipleCandidates` and `ProbableMultipleCandidates` are reported
  rather than silently collapsed.
- **Read-only audit.** `audit_files` takes a slice of `KnownFileEvidence` and a
  `DatIndex`; it never opens, stats, or hashes local files. The CLI enforces this
  by only allowing `--file` comparisons on filenames, not on file contents.
- **Confidence hierarchy.** SHA-256/SHA-1/MD5 produce `Exact*` verdicts; CRC32 +
  exact size produces `Probable`; filename-only produces `FilenameOnly`; no
  evidence produces `NoUsableEvidence`.
- **DAT parser is intentionally outside `safe_read`/`TrustedRoots`.**
  `parsers/mod.rs` explains that a DAT path is typed by the user on the command
  line and does not originate from configuration, manifests, or stored data, so
  trusted-root confinement would refuse ordinary usage while protecting nothing.
  This is a deliberate CLI exception; it stops being an exception the moment a
  DAT path arrives from configuration or stored state.

### 1.3 Current CLI surface

- `archivefs-cli dat inspect <path> [--json]` — parse and print catalogue info.
- `archivefs-cli dat validate <path> [--json]` — parse and report validity,
  warnings, and hash coverage.
- `archivefs-cli dat audit <path> [--file <path> ...] [--json]` — compare named
  files against the DAT by name only; no file contents are read.

There is no GUI integration for DATs today.

### 1.4 Current persistence

DAT parsing has **no persistent storage**. `parse_dat_file`, `DatIndex`, and
`audit_files` are pure in-memory functions. There is no cache, no registry, no
manifest, and no source configuration. The `lib.rs` doc comment explicitly marks
`dat` as "Stage 1A: core model, parsers, indexes, and audit — no persistence or
GUI."

---

## 2. Current cheat-source architecture

### 2.1 Module layout

The RetroArch cheat-source subsystem lives in `archivefs-core::patch_manager` and
its CLI wrappers in `archivefs-cli`:

- `crates/archivefs-core/src/patch_manager/cheat_sources.rs` — registry,
  fetch/verify/extract/publish pipeline, HTTPS transport, URL policy, and
  immutable-snapshot cache.
- `crates/archivefs-core/src/patch_manager/cheat_cache_lock.rs` — cross-process
  advisory lock on the cache root.
- `crates/archivefs-core/src/patch_manager/cheat_cache_maintenance.rs` —
  inventory, verification, pinning, and pruning of immutable snapshots.
- `crates/archivefs-core/src/patch_manager/cheat_catalogue.rs` — local catalogue
  parsing, matching, and availability reports.
- `crates/archivefs-core/src/patch_manager/retroarch_cheat_setup.rs` — guided
  setup orchestration that discovers profiles, resolves a target, and builds a
  plan for the existing installer.
- `crates/archivefs-core/src/patch_manager/cheat_provider.rs` — shared,
  storage-neutral vocabulary for read-only cheat catalogues (`ReadOnlyCheatCatalogue`
  trait) and `CheatProviderSourceState`.
- `crates/archivefs-core/src/patch_manager/cheat_source_registry/mod.rs` —
  **(PR #2)** the read-only registry of all known cheat sources:
  `CheatSourceSpec`, `CheatSourceEntry`, `CheatSourceRegistry`,
  `build_default_registry()`, and per-platform override resolution.
- `crates/archivefs-core/src/patch_manager/cheat_source_registry/config.rs` —
  **(PR #2)** the persisted per-user preferences file (`CheatSourcesConfig`,
  `ProviderConfigEntry`, `PlatformOverrideEntry`, `ProviderPriorityOverride`).
- `crates/archivefs-core/src/patch_manager/cheat_source_registry/capabilities.rs` —
  **(PR #2)** `CheatSourceCapabilities`: what a source can do
  (browse/search/preview/install/download/refresh/health_check/remote/local).
- `crates/archivefs-core/src/patch_manager/cheat_source_registry/health.rs` —
  **(PR #2)** `CheatSourceHealth`, a caller-populated runtime status record. The
  registry never populates it.
- `crates/archivefs-cli/src/cheat_source.rs` — **(PR #2)** CLI for
  `cheat-source list | info | enable | disable | set-priority`.
- `crates/archivefs-cli/src/retroarch_cheat_sources.rs` — CLI wrappers for
  `list`, `fetch`, and `inspect`.
- `crates/archivefs-cli/src/retroarch_cheat_cache.rs` — CLI wrappers for snapshot
  `list`, `verify`, `pin`, and `prune`.
- `crates/archivefs-cli/src/retroarch_cheat_setup.rs` — CLI wrapper for guided
  RetroArch cheat setup.
- `crates/archivefs-cli/src/main.rs` — command dispatch for all the above.
- `crates/archivefs-gui/src/main.rs` — Sources page and Cheats & Mods workspace
  integration.

### 2.2 Key design decisions

- **Compiled-in trusted RetroArch source.** `trusted_retroarch_cheat_sources()`
  returns a single reviewed source (`libretro-buildbot-cheats`) with a
  `download_url` template that requires an exact `{revision}` placeholder. This
  function governs the *retrieval pipeline only*; it is not the source registry.
- **Compiled-in source registry (PR #2).** `build_default_registry()` returns
  **nine** `CheatSourceEntry` values spanning six distinct upstream projects:
  `libretro-buildbot-cheats` (10), `pcsx2-official-patches-tree` (20),
  `gamehacking.org-ps2` (30), `gamehacking.org-gamecube` (40),
  `gamehacking.org-wii` (50), `dolphin_upstream_gamesettings` (60),
  `dolphin_upstream_catalogue` (65), `xenia_canary_game_patches` (70),
  `bsfree-archive` (100). The number in brackets is `default_priority`.
  Duplicate IDs are refused by `CheatSourceRegistry::new`.
- **Priority is `u32` and *lower wins*.** `sorted_all`, `sorted_enabled`, and
  `sorted_enabled_for_platform` all sort *ascending* by priority, breaking ties
  by `SourceId` lexicographic order. Priority `1` is consulted before priority
  `999`. The accepted range is `1..=999`: the CLI **rejects** out-of-range values
  rather than clamping, while `apply_config` **clamps** a configured value into
  the range.
- **Every registry entry defaults to enabled.** `CheatSourceEntry::from_spec`
  sets `enabled: true` and `priority: spec.default_priority` for all nine
  entries; `health` starts as `None`, meaning "not yet checked".
- **The registry carries no trust level.** `CheatSourceSpec` has no trust field.
  `trust_status = "built_in_reviewed"` is computed only by
  `list_retroarch_cheat_sources()` for the retrieval pipeline's single source.
- **Exact revision before download.** The moving `master` reference is resolved
  through the GitHub commits API to a 40-character SHA, then the immutable
  `codeload.github.com/.../zip/<sha>` archive is fetched.
- **HTTPS-only, proxy-disabled, bounded transport.** `HttpsCheatSourceTransport`
  uses `ureq` with `https_only(true)`, `proxy(None)`, `max_redirects(0)`, and
  explicit connect/idle/overall timeouts.
- **Zero automatic redirects; three manual redirects.** Redirects are resolved
  and validated hop-by-hop; credentials, non-HTTPS, non-default ports, and local
  addresses are rejected.
- **DNS-rebinding mitigation.** `validate_public_resolution` resolves the host and
  rejects loopback, private, link-local, unspecified, and documentation addresses.
- **Bounded ZIP extraction.** Extraction refuses absolute paths, `.`/`..`, empty
  components, Windows drive prefixes, backslashes, NULs, oversized paths, deep
  nesting, duplicates, case-fold collisions, symlinks, hard links, special
  files, and unsafe compression ratios.
- **Immutable content-addressed cache.** Snapshots are stored under
  `<cache-root>/<source-id>/snapshots/<archive-sha256>/...`. Manifests live in
  `<cache-root>/<source-id>/manifests/<sha256>.json`. `metadata.json` points to
  the current snapshot.
- **Exclusive cross-process lock.** `LockedCheatCache` holds an advisory
  `flock()` on the cache-root directory; read-only operations still create no lock
  file.
- **Freshness.** A snapshot is fresh for 24 hours; `fetch` reuses fresh cached
  snapshots unless `--force-refresh` is given.
- **Offline mode.** `--offline` reuses a valid snapshot without making a network
  call.
- **Cancellation.** `CheatSourceCancellation` is checked before connect, after
  headers, between chunks, before/during retry waits, before extraction, and
  before activation.
- **Fetch never installs.** The source pipeline produces an immutable snapshot;
  `retroarch-cheat-setup` consumes it and delegates writes to the existing
  journal/backup/installer.

### 2.3 Current CLI surface

Registry and preferences (PR #2):

- `archivefs cheat-source list [--json]` — all nine entries in priority order
  (lowest number first), including disabled ones.
- `archivefs cheat-source info <source-id> [--json]`
- `archivefs cheat-source enable <source-id> [--json]`
- `archivefs cheat-source disable <source-id> [--json]`
- `archivefs cheat-source set-priority <source-id> <1-999> [--json]`

`enable`, `disable`, and `set-priority` **persist immediately** to
`~/.config/archivefs/cheat_sources.toml`; there is no preview or confirmation
step, though the written path is printed. There is no `cheat-source reset`, and
per-platform overrides can only be set by editing the TOML by hand.

Retrieval pipeline:

- `archivefs retroarch-cheat-source-list [--cache-root <path>] [--json]`
- `archivefs retroarch-cheat-source-fetch <source-id> [--force-refresh] [--offline]
  [--expected-sha256 <hash>] [--cache-root <path>] [--max-download-bytes <bytes>] [--json]`
- `archivefs retroarch-cheat-source-inspect <source-id|snapshot-path> [--cache-root <path>] [--json]`
- `archivefs retroarch-cheat-snapshot-list [--source <id>] [--cache-root <path>] [--json]`
- `archivefs retroarch-cheat-snapshot-verify [--all | --source <id> | <snapshot-id>] [--cache-root <path>] [--json]`
- `archivefs retroarch-cheat-snapshot-pin <snapshot-id> [--cache-root <path>] [--json]`
- `archivefs retroarch-cheat-snapshot-unpin <snapshot-id> [--cache-root <path>] [--json]`
- `archivefs retroarch-cheat-cache-prune [--keep <n>] [--older-than-days <d>]
  [--max-cache-bytes <b>] [--include-abandoned-staging] [--abandoned-staging-min-hours <h>]
  [--source <id>] [--cache-root <path>] [--dry-run] [--yes] [--json]`
- `archivefs retroarch-cheat-setup --source <source-id> ...` or
  `archivefs retroarch-cheat-setup <catalogue-path> ...`

### 2.4 GUI integration points

The GUI reuses the same core functions:

- `default_cheat_source_cache_root()` is used for the default cache path.
- `list_retroarch_cheat_sources()` populates the Sources page status.
- `fetch_retroarch_cheat_source()` is driven from the Sources page after a
  review dialog; progress is reported through `CheatSourceProgressReporter`.
- `inspect_retroarch_cheat_source()` and `verify_retroarch_cheat_snapshots()` are
  used for the Verify action.
- `retroarch-cheat-setup` flow is invoked from Cheats & Mods after a source
  snapshot is available.
- The Sources page invalidates dependent preview/confirmation state when a source
  is refreshed.

**The PR #2 registry is not wired to the GUI.** PR #2 touched
`archivefs-gui/src/main.rs` only to make an unrelated doctor-repair test
hermetic. No GUI screen lists the nine registry entries, and enabling,
disabling, or re-prioritising a cheat source is **CLI-only today**. This is the
largest user-control gap the GUI document has to close.

---

## 3. Config and persistence

### 3.1 DAT

- **No config keys.** `config.toml.example` does not mention DAT files.
- **No persistence.** DATs are parsed on demand; no SQLite rows, JSON cache, or
  snapshot files are created.
- **No trust model.** Any path the user provides is parsed. The only trust
  boundary is the user's shell.
- **Limits are hard-coded defaults.** `DatLimits::default()` is used by the CLI;
  `DatLimitsBuilder` exists for callers that want to override values.

### 3.2 Cheat source

- **A per-user preferences file exists (PR #2).**
  `~/.config/archivefs/cheat_sources.toml`, written by
  `save_cheat_sources_config_to` via `atomic_write_text` (temp file + `rename`;
  note it does **not** `fsync`, unlike the cheat-source JSON writers, which do).
  It is separate from `config.toml`, which still does not mention cheat sources.
- **Only non-default values are persisted.** `to_config()` emits an entry only
  when a source is disabled or its priority differs from `default_priority`; an
  absent file means "all nine enabled at their default priorities".
- **The preferences file has no version field.** `CheatSourcesConfig` is
  `#[serde(deny_unknown_fields)]`, so *any* future key — including a
  `format_version` — makes the file unreadable by the current binary. See the
  migration document (§3.2, §4.1): this is the single largest compatibility
  constraint on the whole design.
- **Unknown IDs are silently ignored, then dropped.** `apply_config` skips a
  `providers` entry whose ID is not in the registry, `sorted_enabled_for_platform`
  filters out `priority_overrides` for unknown IDs, and `find_platform_override`
  never matches a platform string that `canonical_platform_for_alias` does not
  resolve. Because `to_config()` re-serialises only live registry entries, the
  next write **deletes** the unmatched entry from the user's file without warning.
  Fixing this is an approved prerequisite bug fix that must ship before any
  schema migration (migration §3.0).
- **Writes are not durable.** `atomic_write_text` does temp file + `rename` with
  no flush at any stage, so a crash can publish a truncated file — which still
  parses as "defaults" and silently discards every stored preference. Approved
  fix: `sync_all` the temp file, rename, then sync the parent directory where
  supported (migration §3.3).
- **Cache root persistence.** Default cache root is derived from
  `default_database_path()`'s parent:
  `~/.local/share/archivefs/cheat-sources/`. `--cache-root` overrides it.
- **Lock file not created.** The lock is held on the cache-root directory
  descriptor via `flock()`.
- **Manifests and snapshots.** `metadata.json`, `manifests/<sha256>.json`, and
  `snapshots/<sha256>/...` are written atomically (temp file + `fs::rename`).
- **Pins.** `pins.json` lives beside the snapshots/manifests directories.
- **Staging cleanup.** `.staging/<pid>-<nanos>/` directories are created during
  fetch and removed on success or on `Drop`.
- **Trust model.** The *retrieval* source list is compiled in and reported as
  `built_in_reviewed`. The PR #2 `cheat_source_registry` carries **no trust
  field at all**, so the nine registry entries currently have no trust
  vocabulary. Future user-added sources are expected to start as `Untrusted` and
  require explicit review; adding that field to the registry is new work, not a
  mapping of something that already exists.
- **Limits are a mix of compile-time constants and CLI overrides.**
  `CHEAT_SOURCE_DEFAULT_DOWNLOAD_LIMIT` (256 MiB) can be lowered by
  `--max-download-bytes`; the registry's `maximum_expected_bytes` is the upper
  bound.

---

## 4. CLI and GUI integration points

### 4.1 DAT

- **CLI only.** `crates/archivefs-cli/src/dat.rs` is the entire CLI surface; it
  calls `parse_dat_file`, `DatIndex::build`, and `audit_files` from
  `archivefs-core::dat`.
- **No GUI.** The GUI does not import `archivefs_core::dat` or offer any DAT
  workflow.
- **No shared preview/installer.** DATs are not connected to the patch/cheat
  installer or to the library catalogue.
- **Output formats.** Human-readable and JSON (`--json`) are implemented in the
  CLI module; there is no shared formatter in core.

### 4.2 Cheat source

- **CLI wrappers are thin.** `retroarch_cheat_sources.rs`,
  `retroarch_cheat_cache.rs`, and `retroarch_cheat_setup.rs` mostly handle
  argument parsing, JSON formatting, and progress printing.
- **Core owns the policy.** URL validation, redirect policy, DNS policy,
  extraction policy, cache path validation, and lock acquisition are all in
  `archivefs-core::patch_manager::cheat_sources` and `cheat_cache_lock`.
- **GUI uses the same core.** The Sources page in `archivefs-gui/src/main.rs`
  calls `list_retroarch_cheat_sources`, `fetch_retroarch_cheat_source`, and
  `inspect_retroarch_cheat_source` directly. Cheats & Mods uses the setup plan
  and the shared installer.
- **Progress reporting is callback-based.** `CheatSourceProgressReporter` is
  passed into the core fetch so the GUI can show phase transitions without core
  knowing about egui.
- **Cancellation is shared.** `CheatSourceCancellation` is the same type used by
  CLI and GUI.

---

## 5. Relevant tests

### 5.1 DAT tests

- `crates/archivefs-core/src/dat/index.rs` — unit tests for index lookup,
  misses, and collisions.
- `crates/archivefs-core/src/dat/audit.rs` — unit tests for exact, probable,
  filename-only, not-in-DAT, and no-evidence verdicts.
- `crates/archivefs-core/src/dat/limits.rs` — unit tests for defaults and
  builder clamping.
- `crates/archivefs-core/src/dat/hash.rs` — unit tests for checksum
  normalisation.
- `crates/archivefs-core/src/dat/parsers/logiqx.rs` and `clrmamepro.rs` —
  parser unit tests embedded in each module.
- `crates/archivefs-core/src/dat/regression.rs` — regression tests for the
  `feature/dat-audit-stage1` fixes, including entity handling, truncation,
  ROM ceilings, duplicate attribute names (RUSTSEC-2026-0194), and audit
  confidence.
- `crates/archivefs-cli/src/dat.rs` — CLI argument and output tests for
  `inspect`, `validate`, and `audit`.

### 5.2 Cheat-source tests

- `crates/archivefs-core/src/patch_manager/cheat_sources.rs` — extensive unit
  tests for registry validation, URL policy, IP policy, fetch/publish/reuse,
  legacy schema compatibility, bounded exclusions, and fake-transport fixtures.
- `crates/archivefs-core/src/patch_manager/cheat_cache_lock.rs` — tests for
  lock acquisition, timeout, release, symlink refusal, prefix collision,
  non-UTF-8 roots, and child-process contention.
- `crates/archivefs-core/src/patch_manager/cheat_cache_maintenance.rs` —
  tests for inventory, verification, pinning, and pruning.
- `crates/archivefs-core/src/patch_manager/cheat_catalogue.rs` — tests for
  catalogue matching and availability.
- `crates/archivefs-core/src/patch_manager/retroarch_cheat_setup.rs` — tests
  for profile discovery and setup plan construction.
- `crates/archivefs-core/tests/retroarch_cheat_install_end_to_end.rs` —
  end-to-end test for the local-catalogue workflow from matching through apply
  and rollback.
- `crates/archivefs-cli/src/retroarch_cheat_sources.rs` — CLI option and list
  JSON tests.
- `crates/archivefs-gui/src/main.rs` — GUI tests for source labels, freshness,
  status transitions, stale-fetch discard, and snapshot fixture rendering.

---

## 6. Recommended places for a shared policy layer

The following policy concerns are implemented independently today, but both DATs
and cheat sources (and other future sources) will need them. A shared layer
should be introduced only where it genuinely reduces duplication without
weakening either subsystem's current guarantees.

### 6.1 Limits and resource ceilings

**Current state:**
- `dat::limits::DatLimits` covers file size, entries, ROMs per entry,
  identifier/description length, warnings, and XML depth.
- `cheat_sources.rs` defines compile-time constants for download size, entry count,
  file size, expanded size, path length, component depth, compression ratio,
  redirect count, retry count, timeouts, and progress events.

**Recommendation:** A shared `archivefs_core::limits` or `policy::resource`
module could define a generic `ResourceLimits` container (max bytes, max count,
max depth, max retries, timeouts) plus a small `LimitBuilder` that clamps to
absolute ceilings. DAT limits and cheat-source limits would remain separate
instances with their own defaults, but the builder/validation logic would be
shared.

**Where to hook:**
- `dat::limits::DatLimitsBuilder` → delegate clamping to shared builder.
- `cheat_sources.rs` constants → keep as subsystem-specific defaults, but use
  shared types for `validate_downloaded_size` and retry delay clamping.

### 6.2 Source registry and trust levels

**Current state:**
- DATs have no registry; any path is accepted.
- Cheat sources have a shipped registry (`cheat_source_registry`, PR #2):
  `CheatSourceSpec` + `CheatSourceEntry` with `enabled`, `priority`,
  `capabilities`, `platforms`, `emulator`, `upstream_project`, and an
  optional caller-set `health`. Preferences persist to
  `~/.config/archivefs/cheat_sources.toml`.
- The registry has **no trust field** and **no verification config**.

**Recommendation:** Do **not** introduce a second registry. The shipped
`cheat_source_registry` is the registry; the remaining work is to add the two
things it lacks and to let DAT sources reuse the same vocabulary:

- `SourceTrustLevel` (`BuiltInReviewed`, `UserTrusted`, `Untrusted`) — a new
  field on `CheatSourceSpec`/`CheatSourceEntry`, defaulting to
  `BuiltInReviewed` for all nine compiled-in entries so nothing changes.
- `SourceVerificationConfig` (pinned hash, signing key) — only meaningful for
  sources with `capabilities.download`.
- `SourceRuntimeState` — `CheatSourceHealth` already occupies this slot; extend
  it rather than replacing it.

`SourceId` already exists as `CheatSourceSpec::id` with uniqueness enforced by
`CheatSourceRegistry::new`. DAT sources could be registered later (e.g. "my
No-Intro DATs directory") so the same trust/review/refresh policy applies.

> REVIEW FIX — the original recommendation proposed a new
> `archivefs_core::source_policy` registry. PR #2 shipped one. Building a second
> would give two competing answers to "is this source enabled?", so this section
> now describes extending the shipped types instead.

**Where to hook:**
- `cheat_source_registry::CheatSourceSpec` → add trust + verification fields.
- `cheat_sources.rs` `CheatSourceDefinition` → remains the *retrieval* definition
  for the one downloadable RetroArch source; it is not the registry.
- Future DAT config persistence → register DAT sources through the same layer.

### 6.3 URL, host, and DNS policy

**Current state:**
- `cheat_sources.rs` has `validate_url_for_source`,
  `validate_public_resolution`, `is_non_public_ip`, and `is_local_hostname`.
- DAT parsing has no network policy because it is local-file-only today.

**Recommendation:** Extract the cheat-source URL policy into a shared
`network_policy` module. The same rules (HTTPS-only, default port, no
auth/credentials, no local/private/link-local addresses, DNS re-resolution) will
be needed by any future metadata provider (ScreenScraper, Hasheous, RomM, etc.).
DAT parsing does not need it now, but if DATs ever arrive from a URL, the shared
policy can be applied.

**Where to hook:**
- `cheat_sources.rs` `validate_url_for_source` / `validate_public_resolution` →
  move to `archivefs_core::net_policy`.
- `archivefs_core::identity_source::net_policy` already exists and is shared
  across identity providers (PR #2 touched it); consolidate onto that module
  rather than creating a third one.

### 6.4 Path safety and trusted roots

**Current state:**
- DAT parser explicitly bypasses `safe_read`/`TrustedRoots` because the path is
  user-typed (see `parsers/mod.rs`).
- Cheat source uses `validate_cache_path_for_read`, `safe_regular_or_directory`,
  `reject_symlink`, and `validate_cache_root_identity` for cache paths.
- ArchiveFS already has `safe_read`/`TrustedRoots` for source-folder reads.

**Recommendation:** When DATs move from user-typed paths to configured or
stored paths, they must use `TrustedRoots` like everything else. The cheat
source's cache-root validation (`validate_cache_root_identity`,
`validate_cache_path_for_read`) is a good candidate to share as a
`CacheRootPolicy` that any disk-based cache can reuse: absolute, non-root, no
symlink components, safe components, safe snapshot names.

**Where to hook:**
- `cheat_cache_lock.rs` `validate_cache_root_identity` → generalise to
  `archivefs_core::cache_policy::validate_cache_root`.
- `cheat_sources.rs` `validate_cache_path_for_read` / `validate_relative_path` →
  move to the same module.
- Future DAT persistence → apply `TrustedRoots` to DAT paths once they are no
  longer user-typed.

### 6.5 Download and transfer limits

**Current state:**
- Cheat source has a complete streaming download pipeline with chunked reads,
  SHA-256 incremental hashing, size limits, and progress reporting.
- DAT has no download pipeline.

**Recommendation:** If DATs or other metadata sources are fetched, the cheat
source's `CheatSourceTransport` trait and `StreamingArchiveWriter` pattern are
a reasonable template. A shared `HttpArtifactFetcher` could own:
- bounded chunked reads,
- incremental hashing,
- progress callbacks,
- Content-Length validation,
- identity/reject-compressed-transfer policy.

**Where to hook:**
- `cheat_sources.rs` `CheatSourceTransport` / `HttpsCheatSourceTransport` →
  generalise to `archivefs_core::fetch::HttpsFetcher`.
- Subsystem-specific fetchers (cheat source, future DAT source) would plug in
  their own URL/redirect/host policies.

### 6.6 Archive extraction and inspection

**Current state:**
- Cheat source extracts ZIPs with strict path validation and unsafe-entry
  rejection.
- DAT parsing does not extract archives.

**Recommendation:** The `extract_zip_safely` logic in `cheat_sources.rs` is the
second archive-inspection layer in the project (after `inspector.rs`). If DATs
are ever distributed as ZIP archives, the same extraction policy should apply.
A shared `archive::zip_policy` could own the per-entry checks: absolute path,
`.`/`..`, drive prefixes, NULs, symlink/special-file rejection, size limits,
compression ratio, and duplicate/case-fold collision.

**Where to hook:**
- `cheat_sources.rs` `extract_zip_safely` → move to shared module.
- Future DAT-delivery archive → reuse.

### 6.7 Verification and manifest provenance

**Current state:**
- Cheat source records `archive_sha256`, exact revision, source URL, canonical
  repository, response metadata, and a sorted per-file manifest.
- DAT has no provenance beyond the `DatSource` struct parsed from the file.

**Recommendation:** A shared `ProvenanceManifest` or `VerifiedSnapshot` type could
record: source ID, fetch URL, resolved immutable URL, SHA-256, timestamp,
verification strength, and per-file digests. The cheat source's
`CheatSourceManifest` would become one consumer; a future DAT import could be
another.

**Where to hook:**
- `cheat_sources.rs` `CheatSourceManifest` / `CheatSourceManifestFile` → derive
  from shared provenance types.
- Future DAT cache → reuse the same manifest envelope.

### 6.8 Audit/confidence vocabulary

**Current state:**
- `dat::audit` has `AuditVerdict` (Exact, ExactMultipleCandidates, Probable,
  ProbableMultipleCandidates, FilenameOnly, Ambiguous, NotInDat,
  NoUsableEvidence).
- `patch_manager` has `MatchConfidence` (Exact, Probable, Uncertain, NoMatch),
  `CheatMatchConfidence`, `ProviderGameMatchConfidence`, etc.

**Recommendation:** Consider a small shared `Confidence` enum or trait in
`archivefs_core::evidence` that captures `Exact` / `Probable` / `Uncertain` /
`NoMatch` plus a `Conflict` state. DAT's `Ambiguous` and
`ExactMultipleCandidates` could be expressed as `Probable + Conflict` or kept as
DAT-specific enrichments. The goal is not to collapse everything into one enum,
but to make the relationship explicit.

**Where to hook:**
- `dat::audit::AuditVerdict` and `patch_manager::MatchConfidence` → document the
  mapping, and optionally introduce a shared trait.

---

## 7. Non-recommendations (things to keep separate)

- **Parser model.** Logiqx/ClrMamePro shapes are DAT-specific and should not be
  forced into a generic source format.
- **Cheat catalogue matching.** `.cht` parsing, platform mapping, and core
  selection are RetroArch-specific and belong in `cheat_catalogue` /
  `retroarch_cheat_setup`.
- **Install/rollback/backup.** The transaction pipeline is intentionally separate
  from source retrieval and should stay that way.
- **XML-specific limits.** XML depth, DOCTYPE handling, and entity handling are
  DAT concerns; do not leak them into generic resource limits.

---

## 8. Summary

The DAT subsystem is a small, read-only, in-memory catalogue parser with no
persistence and no GUI. The cheat-source subsystem is a much larger,
persistence-aware, network-aware pipeline with locks, manifests, snapshots, an
installer boundary, a nine-entry registry, and a per-user preferences file. The
two systems share a number of cross-cutting concerns: resource limits, trust
levels, URL/network policy, path safety, archive extraction, provenance
manifests, and confidence/audit vocabulary.

A shared policy layer should be built incrementally around the already-mature
cheat-source implementations, generalising them where they do not depend on
emulator-specific behaviour. DATs should adopt the shared components as they gain
persistence, configuration, or network access, but should not be forced to fit
into a shape designed for cheat sources today.

The approved next steps, in order (migration §7):

1. **Fix the lossless round-trip** so a preference write can no longer drop an
   unknown provider ID or an unresolved platform override (§3.2 above). This is
   a prerequisite for any schema change.
2. **Make preference writes durable**: file `sync_all`, rename, then
   parent-directory sync where the platform supports it.
3. **Give the shipped registry a GUI** — list, enable/disable, priority, and
   per-platform participation over the existing nine sources. Enable/disable and
   priority are CLI-only today and per-platform overrides are hand-edited TOML
   only; this is a user-control gap, not a modelling gap, and it needs no new
   types and **no new persistence fields**. Steps 1–3 are the first milestone.
4. **Only then** open the schema, behind a released tolerant reader:
   `deny_unknown_fields` means `cheat_sources.toml` cannot grow a key —
   including `format_version` itself — without breaking every released binary
   (migration §3.0, §3.1).
5. **Add trust and verification fields to `CheatSourceSpec`,** defaulting to
   today's behaviour. Trust describes the integration only and must never be
   rendered as an endorsement of upstream content.
6. Extract the genuinely shared, behaviour-neutral policy helpers (URL/DNS
   policy, cache-root path policy) without changing observable behaviour.

DAT adoption would follow once a DAT source registry or persistent DAT cache is
approved. DAT sources are ordered in their own priority space, never against
cheat sources.

---

*Document created: 2026-08-04*
*Scope: current worktree only; no code changes.*
