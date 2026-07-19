# ArchiveFS Patch & Cheat Manager Design

## Status and Scope

This document defines the future architecture and records the current Phase 1 implementation boundary. It does not authorize a database migration or any change to ArchiveFS mount behavior.

The current uncommitted implementation contains the first adapter slice: a read-only PCSX2 metadata preview. It fetches one compiled-in metadata endpoint into bounded memory, reports native and Flatpak standard-path **installation candidates**, reads the existing catalogue through an enforced read-only connection, and produces a non-executable advisory plan. A candidate means only that a documented standard directory exists; Phase 1 does not prove that an emulator binary is installed, identify its version, validate a complete layout, inspect destination contents, or establish mutation authority.

RetroArch is the recommended next adapter, but it is not implemented or scaffolded in Phase 1. Dolphin, RPCS3, PPSSPP, DuckStation, Xenia, Azahar/Citra-compatible 3DS emulators, legally and technically appropriate Ryujinx-compatible Switch layouts, MAME, ScummVM, DOSBox variants, and all other adapters are explicitly deferred. Their names below describe architectural intent, not current support.

Exact catalogue matching is currently blocked in production because the approved `PersistedArchive` read model exposes no PS2 disc serial, executable CRC, or cryptographic game identity. The matcher contains conservative exact-evidence rules and synthetic unit fixtures, but real catalogue rows provide only platform/title evidence and filename text; filename text is never promoted to `Exact`. The selected PCSX2 metadata repository also has no declared repository-wide patch license. Metadata-only evaluation may continue with that limitation displayed, but redistribution, artifact download, installation, caching, or bundling remains blocked pending licensing and source-policy approval.

The Patch & Cheat Manager is a managed system for discovering, previewing, downloading, installing, updating, auditing, and rolling back emulator cheats and patches. Candidate content includes cheat databases, widescreen and 60 FPS patches, translation patches, emulator compatibility patches, controller fixes, and small community patch formats such as IPS, BPS, and xdelta metadata. Users may configure additional source URLs.

The manager must never request, accept into its trusted cache, install, or distribute complete copyrighted ROMs, ISOs, disc images, or games. It requests only reviewed metadata and narrowly allowlisted patch content types, applies strict byte limits, stops a mismatched stream as soon as it can identify it, and deletes the untrusted partial payload while retaining only a bounded rejection record. No client can prevent a malicious server from sending mislabeled game bytes after a legitimate patch request, so the honest enforceable boundary is that such bytes are bounded, never accepted as a patch, never installed, never redistributed, and never retained as content. Phase 1 avoids this exposure entirely by requesting metadata only. The manager does not patch an ArchiveFS source archive in place. Binary patch application to a user-supplied game, if ever supported, is a separately reviewed future capability and must produce a new derived file rather than alter the original archive.

The overall first release may eventually fetch and inspect small non-executable artifacts and manage emulator-owned cheat or patch files. The **implemented gate is narrower than that release**: Phase 1 is only an in-memory fetch and read-only PCSX2 metadata preview. It does not persist source configuration or cache data, download artifact payloads, create installation plans that can be executed, write audit records, install files, create backups, or expose rollback. No production implementation beyond that Phase 1 boundary may begin without a new design approval. Mount and unmount execution remains independent in every phase.

## Design Principles

- Downloading, inspection, planning, installation, and presentation are separate layers.
- Remote names, paths, metadata, archives, and signatures are untrusted input.
- Preview is read-only. A metadata-only preview is advisory and cannot be executed; only a later artifact-backed preview can describe a complete proposed change set.
- The generic manager owns provenance, verification, limits, plans, manifests, backups, and transactions; emulator adapters own emulator-specific paths and formats.
- Original game archives are immutable inputs. Catalogue metadata may identify a game without changing its archive.
- Every mutation must be attributable, reversible where technically possible, and recoverable after interruption.
- A missing optional dependency degrades only its declared capability.
- CLI and GUI present the same plan and safety state from shared core types.

## End-to-End Trust and Safety Model

The required state machine is:

```text
source configuration
  -> metadata fetch
  -> temporary download
  -> checksum/signature verification when available
  -> archive inspection
  -> path traversal validation
  -> preview
  -> backup
  -> atomic install
  -> manifest update
  -> rollback
```

Each transition produces a typed result. In phases that persist audit data, it also produces a sanitized audit event; Phase 1 keeps diagnostics in memory so its preview remains filesystem-read-only. Failure stops the transition, and later stages cannot infer success from the presence of a partially written file.

1. **Source configuration:** validate the URL scheme, source identity, declared format, trust policy, destination adapter, limits, and authentication reference. HTTPS is the default and required for unattended retrieval. A local-file or HTTP override is outside Phase 1; if later approved, it is manual-only, visibly lowers trust, and prevents unattended installation.
2. **Metadata fetch:** retrieve into bounded memory in Phase 1 and, only in a later phase, into a private manager cache. Apply connect/read/overall timeouts, redirect and decompressed-response-size limits, bounded parser depth and collection sizes, duplicate-key policy, and content-type diagnostics. Content type is not a security boundary. Resolve and revalidate every redirect hop; never downgrade scheme, forward credentials across origins, or redirect to loopback, link-local, private, Unix-socket, or other local resources unless a separate explicit local-source policy allows that exact target. Recheck resolved addresses on connection to reduce DNS-rebinding exposure.
3. **Temporary download:** in Phase 2 or later, stream artifact bytes to a newly created private manager staging directory, not an emulator or game directory. Enforce transferred and decoded byte limits while streaming. Only after approval may an install transaction create same-filesystem destination staging. Never execute, source, import as code, or open downloaded content through a shell.
4. **Verification:** compute a cryptographic artifact hash unconditionally. Verify a pinned checksum or signature when the source supplies one. A missing or invalid stronger verification mechanism must never be silently downgraded to an unverified download.
5. **Archive inspection:** parse with a built-in Rust reader where possible, without extraction. Enforce compressed size, declared and observed expanded size, compression ratio, nesting depth, entry count, per-entry size, and total expanded-size limits. Reject executable formats and scripts in every source type. A source type may separately allow narrowly identified binary *data* patch formats such as IPS/BPS, but those bytes are never executable content.
6. **Path validation:** normalize each archive entry as a platform-neutral relative path. Reject absolute paths, drive or UNC prefixes, empty/ambiguous components, `.` and `..`, NULs, reserved destination names where relevant, duplicate normalized paths, and paths exceeding configured limits. Reject symlinks, hard links, device nodes, FIFOs, sockets, and other special entries by default.
7. **Preview:** resolve game identity and ask the selected adapter to validate formats and destinations. A metadata-only input produces a non-executable `AdvisoryPatchPlan`; an inspected artifact may produce an execution-eligible `PatchPlan`. Planning makes no filesystem changes, including no cache write, destination directory creation, lock, backup, manifest, journal, or persistent audit allocation.
8. **Backup:** after explicit approval, acquire the installation-namespace lock, revalidate every preview precondition through race-resistant filesystem handles, and durably publish a `Prepared` operation journal before creating staging or backup objects. Copy every existing regular destination file that will be replaced into a private content-addressed backup area, sync it, hash the bytes actually copied, and confirm that the opened destination still has the planned identity and hash before replacement. Record each durable backup and its metadata in the journal. User confirmation is mandatory before replacing any file unless an exact-match unattended policy was explicitly enabled and all other safety requirements pass.
9. **Atomic install:** stage complete replacement files on the destination filesystem, verify hashes, then atomically rename them relative to already opened directory handles. Creation uses no-replace semantics. Replacement should use atomic exchange so the displaced object can be verified against the planned old identity/hash after the swap; a mismatch triggers an immediate journaled exchange-back attempt and then `RecoveryBlocked`, never continued installation. Do not resolve mutation targets again from unchecked path strings. On Linux, prefer directory-relative no-follow operations and `openat2`/`renameat2` constraints where available; equivalent protections require per-platform review. If the filesystem cannot provide required no-replace/exchange, durable journal, or no-follow semantics, replacement is unsupported there. A multi-file install is journaled and recoverable but is never described as transactionally atomic as a group.
10. **Manifest update:** record the exact root-relative path and installed hash of every created or replaced file. Publish a new immutable manifest generation through a create-new temporary file, sync, validated rename to its generation name, and parent sync. Then atomically replace and sync the small active-generation reference; never overwrite the predecessor generation. The journal is then durably marked committed; it is not deleted as part of commit and is reclaimed only by a later safe garbage-collection pass.
11. **Rollback:** operate only on manifest-owned installed paths and referenced backups. Verify current installed hashes before removing or replacing anything. A locally changed file is reported and left untouched unless the user explicitly approves its replacement. Rollback itself is journaled and recoverable.

The current Phase 1 client is intentionally narrower than the general redirect policy: it accepts exactly the compiled-in HTTPS URL, disables proxies, follows zero redirects, requests identity encoding, rejects any non-identity `Content-Encoding`, and bounds the received body at 8 MiB. Consequently there is no redirect-hop or decompression path in the implementation. The fixed-origin TLS connection reduces the Phase 1 SSRF surface, but the current tests do not provide a custom resolver/connector, connected-peer verification, local TLS server, or DNS-rebinding simulation. Those remain release-hardening work rather than guarantees established by the existing suite.

Downloaded data is never interpolated into a command line. The manager must not construct shell commands from downloaded data, and it must not automatically execute downloaded programs, scripts, installers, macros, or post-install hooks. Library APIs and argument-vector process APIs are distinct from shell evaluation, but optional process invocation is permitted only for a reviewed, locally configured external tool and only with manager-generated arguments.

Global defaults must include finite transferred and decoded download size, metadata nesting and collection size, file-count, expanded-size, per-file, compression-ratio, path-length, archive-depth, redirect, memory, concurrency, and operation-time limits. Limits are source-type aware but may only be raised through explicit local configuration. Interrupted fetches and inspections leave only untrusted private temporary objects; interrupted installs, manifest writes, and rollbacks leave a durable journal. Recovery follows explicit hash-based rules below and never guesses whether to resume or reverse.

## Source Configuration

Source configuration is distinct from fetched source metadata. A proposed core model is:

```rust
pub struct PatchSourceConfig {
    pub id: SourceId,
    pub display_name: String,
    pub url: RedactedUrl,
    pub source_type: PatchSourceType,
    pub targets: Vec<SourceTarget>,
    pub enabled: bool,
    pub update_interval: UpdateInterval,
    pub verification: SourceVerificationConfig,
    pub expected_content: ExpectedContent,
    pub trust_level: SourceTrustLevel,
    pub destination_adapter: AdapterId,
    pub authentication: Option<AuthStrategyRef>,
}

pub struct PatchSource {
    pub config: PatchSourceConfig,
    pub runtime: SourceRuntimeState,
}

pub struct SourceRuntimeState {
    pub last_successful_update: Option<Timestamp>,
    pub last_failure: Option<SourceFailureSummary>,
    pub last_accepted_version: Option<SourceVersion>,
    pub strongest_verification: Option<VerificationStrength>,
}

pub struct SourceTarget {
    pub emulator: Option<AdapterId>,
    pub platform: PlatformId,
}

pub enum PatchSourceType {
    StaticIndex,
    VersionedManifest,
    CheatDatabase,
    AdapterNativeRepository,
}

pub enum ExpectedContent {
    JsonManifest { schema: String },
    TomlManifest { schema: String },
    Zip { allowed_formats: Vec<PatchFormat> },
    Tar { allowed_formats: Vec<PatchFormat> },
    SingleFile { format: PatchFormat },
}

pub enum SourceTrustLevel {
    BuiltInReviewed,
    UserTrusted,
    Untrusted,
}

pub enum AuthStrategyRef {
    None,
    EnvironmentVariable { name: String },
    KeyringEntry { service: String, account: String },
    ExternalCredentialHelper { helper_id: String },
}
```

`SourceId` is stable across display-name and approved endpoint changes and must not be derived only from a mutable URL. Stability does not transfer trust: changing scheme, origin, base path, authentication strategy, or verification material disables the source, clears unattended eligibility, and requires explicit re-approval while retaining the ID only for audit continuity. `SourceVerificationConfig` can pin a manifest hash, artifact checksum URL, public signing key or key fingerprint, signature URL template, and downgrade policy. Checksum and signature endpoints receive the same scheme, origin, redirect, credential, and network-target validation as the primary URL. `SourceFailureSummary` stores a sanitized category, timestamp, and non-secret diagnostic rather than response bodies or credentials. Source versions combine an upstream immutable identifier, when present, with the fetched metadata hash; a timestamp alone is not a version. Mutable operational fields live in `SourceRuntimeState`, not in user-authored configuration.

Secrets are never stored in plain-text source configuration, manifests, logs, GUI state snapshots, or error messages. Configuration stores only a reference to a process environment variable, OS keyring entry, or separately reviewed credential helper. Credentials are scoped to an origin and are stripped on cross-origin redirects.

Built-in sources are not intrinsically safe: they begin with reviewed defaults and may pin verification material, but their responses remain untrusted. Every user-added URL starts as `Untrusted`, has unattended updates disabled, uses conservative limits, and requires the user to review its host, formats, requested adapter, verification status, and every initial plan. Trust may be raised only by explicit local action; prior success does not automatically raise it.

Source state may live in manager-owned files beginning in a later approved phase, not in the ArchiveFS catalogue schema. Phase 1 uses one compiled-in source definition and does not write source state. User-added sources, source editing, update scheduling, and persistence are explicitly outside Phase 1. Configuration parsing integration and the persistence format require a separate implementation design review.

## Retrieval and Verification

`SourceClient` retrieves only metadata and artifacts declared by a validated source. It uses a dedicated user agent, TLS validation, bounded concurrency, timeouts, and the network-target restrictions above. Phase 1 returns a bounded in-memory `MetadataSnapshot` and has no persistent cache. A later cache is private, content-addressed, and keyed by source ID, approved canonical URL, validators, and content hash; cache metadata records verification strength and policy at acquisition. Offline mode may use only a complete cached object whose hash revalidates and whose recorded verification strength still satisfies current policy, and it clearly reports acquisition time and age.

Verification results distinguish:

- `SignatureVerified`: a signature chains to a locally trusted pinned key.
- `ChecksumPinned`: the expected digest came from local trusted configuration.
- `ChecksumAuthenticated`: the digest came from independently authenticated signed metadata.
- `TransportOnly`: HTTPS protected transport, but no artifact-level proof exists.
- `Unverified`: no acceptable integrity claim exists.
- `VerificationFailed`: a provided or required integrity claim did not validate.

Metadata snapshots and artifacts are always hashed locally, even when unverified. A checksum downloaded from the same unsigned trust domain as the artifact detects accidents but not a compromised server and must not be described as authenticated. If a source previously used a signature or authenticated checksum, loss of that signal blocks acceptance; “explicit review” means changing local verification policy, not clicking through an individual update. Metadata version counters, publication timestamps, or signed expiry information should prevent silent replay where the source format supports them. Without authenticated monotonic version or expiry data, the manager can detect only locally observed rollback; it must state that first-seen replay remains unresolved.

## Safe Artifact Inspection

`ArtifactInspector` works in a private staging area and produces an immutable `InspectedArtifact`. It inventories entries, types, sizes, normalized paths, hashes, detected formats, and rejection reasons. Inspection is separate from destination mapping.

Policy includes:

- reject absolute, rooted, drive-qualified, UNC, parent-traversing, and normalization-ambiguous paths;
- reject symlinks and special files, and never follow links in the staging or destination trees;
- reject duplicate or case-colliding destination paths on filesystems where they would alias;
- reject unsupported nesting and recursively compressed content unless the source type explicitly requires bounded nesting;
- compare declared sizes with streamed observed sizes and stop at the lower applicable safety limit;
- reject content that resembles a complete game image or exceeds the source type's expected patch size;
- identify executable formats, scripts, and archive post-install metadata and reject them for every supported source type;
- retain inspection reports and hashes, not unbounded payload data, in audit records.

IPS, BPS, and xdelta descriptors may be indexed and validated initially without applying them. If application is later added, the patch must declare expected input and output hashes, the user must supply the input, the original remains read-only, and output goes to a new user-approved location.

## Emulator Adapter Architecture

Emulator-specific behavior is kept outside source retrieval, verification, archive inspection, transaction journaling, backup storage, and manifest serialization.

```rust
pub trait EmulatorAdapter: Send + Sync {
    fn id(&self) -> AdapterId;
    fn capabilities(&self) -> AdapterCapabilities;
    fn discover_installations(&self, context: &DiscoveryContext)
        -> Result<Vec<EmulatorInstallation>, AdapterError>;
    fn collect_game_evidence(
        &self,
        context: &ReadOnlyGameContext,
        installation: &EmulatorInstallation,
    )
        -> Result<Vec<AdapterGameIdentity>, AdapterError>;
    fn plan_advisory(&self, request: &AdvisoryAdapterRequest)
        -> Result<Vec<AdvisoryAdapterEntry>, AdapterError>;
    fn validate_advisory(&self, plan: &AdvisoryAdapterPlan)
        -> Result<AdapterValidation, AdapterError>;
    fn health_check(&self, installation: &EmulatorInstallation)
        -> Vec<AdapterHealthCheck>;
}

pub trait EmulatorMutationAdapter: EmulatorAdapter {
    fn install_recipe(&self, request: &ArtifactBackedAdapterRequest)
        -> Result<InstallRecipe, AdapterError>;
    fn rollback_recipe(&self, manifest: &InstallManifest)
        -> Result<RollbackRecipe, AdapterError>;
}
```

The base `EmulatorAdapter` is read-only and is the only adapter trait available in Phase 1. A later adapter opts into mutation by implementing `EmulatorMutationAdapter`. Its install and rollback behavior is expressed as declarative recipes containing manager-generated bytes or a verified staged-content reference, an approved root ID, safe relative destinations, expected prior hashes, file modes, and restart/rescan notices. The generic transaction executor—not the adapter—opens paths, writes, renames, backs up, updates manifests, or recovers. Recipes cannot request commands, arbitrary callbacks, absolute paths, deletion of unowned objects, or access outside registered roots. This preserves emulator-specific mapping while keeping mutation authority in one auditable layer.

`ReadOnlyGameContext` exposes only a catalogue snapshot and explicitly approved read-only discovery roots; Phase 1 does not expose arbitrary filesystem or mutable catalogue handles to an adapter. `EmulatorInstallation` contains a stable installation ID, adapter/version/variant evidence, discovery provenance, and one or more `ApprovedRootDescriptor` values. A root descriptor identifies its purpose, canonical path for display, filesystem/device identity, and whether it was explicit or discovered; canonical strings alone are not security capabilities. `AdapterCapabilities` declares supported formats, global/per-game scope, identity methods and namespaces, restart/rescan requirements, supported emulator versions, discovery confidence, and whether mutation recipes are implemented.

Initial adapter responsibilities are:

| Adapter | Path discovery | Formats and scope | Game identity | Restart/rescan and validation |
| --- | --- | --- | --- | --- |
| `RetroArchAdapter` | Discover explicit config plus standard Linux/Flatpak data and config roots; never guess a writable root when several exist | Core-specific `.cht`, data files, and approved patch descriptors; global databases and per-content files | Content CRC/hash, playlist database fields, core/system, and catalogue identity | Declare core/content reload needs; validate core association, format, and configured writable roots |
| `Rpcs3Adapter` | Explicit root plus standard native/Flatpak RPCS3 config locations | RPCS3 patch YAML and approved cheat data; primarily per-title/version with some global patch indexes | PS3 title ID, serial, app/version metadata, and hashes where defined | Patch rescan or emulator restart as detected; validate title/version selectors and YAML schema without launching RPCS3 |
| `Pcsx2Adapter` | Phase 1 checks only the standard Linux XDG native candidate and `net.pcsx2.PCSX2` Flatpak candidate. It has no explicit-root override yet, does not inspect binaries or versions, and never chooses between candidates | PNACH cheats, widescreen and no-interlacing patches are future capabilities. Phase 1 parses repository tree metadata only and does not read or write PNACH payloads | Namespaced PS2 disc serial and executable CRC extracted from supported metadata filenames; the current catalogue supplies neither as approved identity evidence and Phase 1 does not hash game content | Phase 1 validates the fixed Git-tree response shape, Git IDs, repository paths, and hypothetical `patches/<name>.pnach` paths. Version support, payload grammar, enablement, destination contents, writability, restart behavior, and mutation readiness are `NotEvaluated` |
| `DolphinAdapter` | Explicit user directory plus standard native/Flatpak paths | Game INI patches, Gecko/Action Replay codes, Riivolution metadata where safely supported; per-game and global config | Game ID, revision, region, and hashes where useful | Config reload/restart requirements; validate INI sections, IDs, and safe referenced paths |
| `XeniaAdapter` | Explicit installation/content roots first; conservative discovery of known config layouts | TOML patch/cheat definitions supported by the detected Xenia variant; per-title and global repositories | Xbox title ID, media ID, version, and hashes where available | Usually restart/rescan per detected capability; validate variant/schema/title selectors |
| `CustomFolderAdapter` | User-selected, canonicalized allowlisted root only | User-allowlisted extensions and layouts; global or per-game mapping chosen in config | Explicit identifiers or catalogue mapping; never filename-only unattended matching | No assumed rescan; user-configured notice, strict relative path map, and root health checks |

Installation discovery is read-only and yields confidence and provenance. Explicit user targets take precedence but are still validated. Multiple installations and ambiguous candidate roots remain distinct and block a single-target plan until selected; no adapter silently chooses. Discovery must not create a standard directory merely because it is absent. General health checks may verify target presence, permissions, filesystem mutation capabilities, version compatibility, path containment, pending journals, and restart/rescan state, but Phase 1 health is restricted to read-only existence/version/provenance observations and reports mutation capability as `NotEvaluated`. Health checks must not start, stop, signal, or control an emulator.

Before any adapter gains mutation support, its versioned profile must specify and test all of the following:

- authoritative explicit and discovered roots, precedence, ambiguity behavior, and sandbox/package variants;
- exact supported emulator versions and a fail-closed result for unknown layouts;
- format parser/schema version, encoding/newline rules, maximum sizes, and whether unknown directives are preserved or rejected;
- global versus per-game mapping, identity namespace/revision requirements, normalized destination naming, and collision behavior;
- whether files are manager-owned standalone files or shared user configuration. Shared-file editing is unsupported until a lossless, format-aware merge model can preserve comments, order, unknown fields, concurrent edits, and byte-for-byte rollback;
- preview validation and every condition that blocks a recipe;
- install recipe, created/replaced directory and file semantics, mode/metadata policy, and ownership transfer behavior;
- rollback recipe derived only from the manifest generation and verified backups, never from current source metadata;
- restart/rescan requirements and whether running-emulator detection is reliable, advisory, or unavailable;
- health checks and Doctor capabilities, with read-only checks separated from mutation readiness.

The table above is a roadmap, not a claim that those profiles or formats are already supported. PCSX2 is the first implemented read-only candidate-discovery and metadata slice. RetroArch now has its own implemented read-only cheat/patch destination preview (`retroarch-patch-preview`, see [`docs/RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md)) - **but it is not an `EmulatorAdapter` implementation**. The provisional, narrowly-scoped neutral adapter extraction described below remains PCSX2-only; RetroArch's own preview is a separate module (`patch_manager::retroarch`) with its own `RetroArchAdvisoryPlan` type, built precisely because the gaps listed below turned out to be real: forcing RetroArch through `EmulatorAdapter`/`AdvisoryPatchPlan` would have required either weakening that trait/type for PCSX2 or silently misrepresenting RetroArch's genuinely different shape. Dolphin, RPCS3, PPSSPP, DuckStation, Xenia, Azahar/Citra-compatible 3DS emulators, Ryujinx-compatible Switch layouts, MAME, ScummVM, DOSBox variants, and all other adapters remain unimplemented; their names below describe architectural intent, not current support.

A follow-up milestone added read-only RetroArch playlist (`.lpl`) discovery, parsing, and catalogue matching - see [`docs/RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md). This did not change the `EmulatorAdapter`-vs-independent-module decision above: playlist discovery lives in `emulator_environment::retroarch` (reusing that module's own resolved-directory map), and playlist-to-catalogue matching lives in `patch_manager::retroarch` as additional, purely additive evidence (`playlist_evidence`/`selected_core_source` on `RetroArchProfileOutcome`) that can strengthen or resolve an ambiguous extension-based core selection, but never overrides an already-correct one. No `patch_manager` type outside `retroarch.rs` was touched, and no PCSX2 behavior changed.

**Provisional neutral adapter seam.** `patch_manager::adapter` defines a minimal read-only contract covering only what Phase 1's actual PCSX2 slice needs: `EmulatorAdapter` (`id`, `capabilities`, `discover_installations`, `identity_evidence_from_record`, `identity_evidence_from_catalogue`, `hypothetical_relative_path`), `AdapterId`, `AdapterCapabilities`, `InstallationCandidate`, `AdapterIdentityEvidence`, and `HypotheticalDestination`. `patch_manager::matching` holds one small function, `exact_tier_outcome`, that compares namespaced identity evidence from both sides without hardcoding what a "PS2 serial" or "executable CRC" is. `ReadOnlyPcsx2Adapter` (`patch_manager::pcsx2`) is the first, and so far only, implementation of `EmulatorAdapter`: it owns candidate discovery and conversion, PS2 serial/executable-CRC normalization, PNACH filename identity parsing, and validated `patches/<name>.pnach` hypothetical destination calculation (independently rejecting traversal, nesting, absolute paths, backslashes, empty filenames, and wrong extensions, not merely reformatting whatever it is given). `AdvisoryPatchPlan.installation_candidates` and `AdvisoryPlanEntry.hypothetical_destinations` are now the neutral `InstallationCandidate`/`HypotheticalDestination` types.

This is a seam, not a generalized framework: `patch_manager::mod` remains the PCSX2-specific *orchestration* layer for this milestone - platform filtering (`"PS2"`), the title/region "probable" heuristic, the filename-similarity "uncertain" heuristic, confidence aggregation, ambiguity detection, and plan/plan-ID assembly are not adapter-parameterized. A second adapter cannot yet be added without first generalizing that orchestration and separately reviewing its own explicit-root/multi-root/multi-core-selection profile; nothing here schedules or scaffolds that work.

**What is actually verified, and by what.** A fixed regression suite (golden plan-ID hashes reproduced from the pre-extraction implementation at commit `52d6ef5`, an exact JSON key-set assertion, an exact human-readable output comparison, and a complete `format_version = 1` JSON candidate-object comparison) checks, for the specific fixed scenarios it covers: command name, human-readable output text, JSON field names and values (`installation_candidates[].kind`/`hypothetical_destinations[].candidate_kind` remain plain strings like `"Native"`/`"Flatpak"`, with no new `adapter_id` key leaking into `format_version = 1` output), candidate ordering, matching outcomes, ambiguity handling, and canonical plan IDs. The read-only/no-migration behavior is checked by separate fixture tests that open a real database, run a preview, and compare schema version and file bytes before and after - one test for byte/path preservation, a distinct one for schema-version preservation - proving fixture stability and the structural absence of any write/migration call in the code path exercised, not an exhaustive proof that no code path anywhere could ever write.

Patch retrieval itself is not capability-free: `retrieval.rs`'s `HttpsMetadataFetcher` makes a real network call to the one compiled-in, fixed HTTPS endpoint, exactly as it did before this extraction - that pre-existing behavior, its limits, and its verification level are all unchanged, because `retrieval.rs` was not touched by this extraction. What *is* accurate to claim, and is true by inspection, is narrower: `adapter.rs`, `matching.rs`, and the PCSX2 adapter seam in `pcsx2.rs` (discovery, capability declaration, identity evidence, and hypothetical-destination derivation) expose no process, shell, network, privileged-command, mutation, or write capability - no such call exists in any of those three, confirmed by inspection, not merely asserted. This is not the same claim as a complete, continuously-enforced capability-log or sandboxing guarantee across the whole `patch_manager` module, and none is claimed here.

Known RetroArch gaps this extraction did not attempt to close, and how `retroarch-patch-preview` actually addressed each (see [`docs/RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md) for the full record):

- **Profiles**: still not a generic, versioned discovery/format/identity profile in the sense this document's "Before any adapter gains mutation support" checklist describes (that checklist is mutation-readiness scoped; this preview remains read-only). RetroArch's own three fixed profiles (native, Flatpak user, Flatpak system) are reused as-is from `emulator_environment::retroarch`, not re-derived.
- **Multiple roots per installation**: resolved by *not* using `InstallationCandidate` (still exactly one `data_root`) for RetroArch at all. `patch_manager::retroarch` embeds the full `RetroArchEnvironmentReport` - which already models twelve purpose-tagged paths per profile - directly in its own `RetroArchAdvisoryPlan`, rather than projecting it down to one root.
- **Core inventory**: resolved the same way - reused directly from `RetroArchEnvironmentReport.profiles[].cores[]` (already inventoried, with `.info` metadata), not re-derived.
- **Core ambiguity**: resolved with a RetroArch-specific `CoreMatchDisposition` (`ExactCore`/`AmbiguousCore`/`UnsupportedNoCore`/...), distinct from `AdvisoryDisposition::AmbiguousInstallationCandidates` (which remains PCSX2-only and unchanged) - exactly one installed core supporting the catalogue archive's file extension yields `ExactCore`; two or more yields `AmbiguousCore` with no single destination proposed; zero yields `UnsupportedNoCore`.

## Game Identity and Matching

`GameIdentity` collects evidence without making a match by itself:

```rust
pub struct GameIdentity {
    pub platform: PlatformId,
    pub serials: Vec<String>,
    pub title_ids: Vec<String>,
    pub crcs: Vec<CrcValue>,
    pub cryptographic_hashes: Vec<ContentHash>,
    pub normalized_title: Option<String>,
    pub region: Option<Region>,
    pub catalogue_record: Option<CatalogueRecordId>,
}

pub enum MatchConfidence {
    Exact,
    Probable,
    Uncertain,
    NoMatch,
}
```

Confidence is evidence based:

- **Exact:** all identifiers required by the source record agree in the same adapter/platform namespace, at least one compatible stable identifier agrees—serial, title ID, emulator-defined CRC, or cryptographic hash—and no strong identifier conflicts. CRC width/algorithm and its emulator meaning are part of its namespace; a bare integer is not exact evidence. A serial is not sufficient when a patch declares a required executable CRC or revision. Conflicting exact identifiers produce `Conflict`, not `Exact`.
- **Probable:** normalized title, platform, and region agree, with no contradictory strong identifier. A missing region may reduce rather than improve confidence.
- **Uncertain:** only filename similarity or incomplete title evidence agrees.
- **NoMatch:** required platform or identifiers disagree, or no useful evidence exists.

An otherwise exact record that resolves to multiple local games or installations is `Conflict`, not several exact actionable matches. Identity normalization is adapter/version-specific and preserves raw evidence for display and audit. Title similarity can never override a strong mismatch.

Only `Exact` matches may be eligible for unattended updates, and only when source trust, verification, manifest ownership, unchanged destination hashes, and local unattended policy also permit it. `Probable` and `Uncertain` always require explicit review. Phase 1 has no unattended or execution capability regardless of confidence. The interface displays the evidence, namespace, provenance, required identifiers, and contradictions, not only a score. It must never silently install an uncertain game match.

The ArchiveFS catalogue can contribute platform, normalized title, region, serial/title ID, and hash metadata already known for an archive. The matcher reads a consistent catalogue snapshot; it does not hold catalogue locks while performing network work. A future explicit hash operation may read game bytes and store results outside the game archive, but that operation is not part of preview and is outside Phase 1. The matcher does not rename, rewrite, repack, extract into, mount, or otherwise alter the game archive. A catalogue record is supporting evidence, not proof unless it carries an exact identifier appropriate to that emulator, patch format, region, and revision.

## Preview and Planning

Planning consumes source metadata and, in later phases, an inspected artifact, adapter discovery, catalogue evidence, existing manifests, and destination observations. It returns one of two stable, serializable plan types with no side effects. The type boundary prevents a metadata preview from being passed to an executor.

```rust
pub struct AdvisoryPatchPlan {
    pub plan_id: PlanId,
    pub source: SourceSnapshot,
    pub metadata_snapshot_hash: ContentHash,
    pub target: Option<EmulatorInstallationId>,
    pub generated_at: Timestamp,
    pub entries: Vec<PlanEntry>,
    pub summary: PlanSummary,
}

pub struct PatchPlan {
    pub plan_id: PlanId,
    pub source: SourceSnapshot,
    pub metadata_snapshot_hash: ContentHash,
    pub artifact_hash: ContentHash,
    pub target: EmulatorInstallationId,
    pub generated_at: Timestamp,
    pub preconditions: Vec<PlanPrecondition>,
    pub entries: Vec<ExecutablePlanEntry>,
    pub summary: PlanSummary,
}

pub struct PlanEntry {
    pub action: PlanAction,
    pub disposition: PlanDisposition,
    pub match_result: Option<GameMatch>,
    pub hypothetical_destination: Option<SafeRelativePath>,
    pub reasons: Vec<PlanReason>,
}

pub struct ExecutablePlanEntry {
    pub action: PlanAction,
    pub disposition: PlanDisposition,
    pub match_result: GameMatch,
    pub source_entry: SafeRelativePath,
    pub destination: ApprovedDestination,
    pub current_hash: Option<ContentHash>,
    pub proposed_hash: ContentHash,
    pub reasons: Vec<PlanReason>,
    pub confirmation: ConfirmationRequirement,
}
```

`AdvisoryPatchPlan` is the only plan produced in Phase 1. Its `PlanEntry` may show a hypothetical safe relative destination, but has no approved destination capability, proposed payload hash, preconditions for mutation, confirmation token, or conversion method to `PatchPlan`. `PatchPlan` and `ExecutablePlanEntry` are introduced only with inspected artifact payloads and the transaction phase.

The implemented `AdvisoryPatchPlan` is a smaller PCSX2-first representation. It serializes a format version, deterministic plan ID, explicit `executable: false`, source snapshot, every installation candidate, entries, and summary. Its plan ID hashes canonical decision inputs and exact candidate path bytes; it excludes presentation reasons and lossy display paths. Candidate roots and combined PNACH display paths are informational strings, not approved destinations or filesystem capabilities. Multiple candidates produce `AmbiguousInstallationCandidates`; no standard-path candidate produces `NoInstallationCandidate`, which does not claim that PCSX2 is absent from the machine.

The eventual action/disposition/reason vocabulary must represent at least: `Install`, `Update`, `AlreadyCurrent`, `Conflict`, `UnsafePath`, `Unsupported`, `MissingEmulator`, `MissingGame`, `ExactMatch`, `ProbableMatch`, `UncertainMatch`, `VerificationFailed`, `RemovalCandidate`, and `RollbackAvailable`. Phase 1 implements only preview, missing-game, no-candidate, ambiguous-game, ambiguous-candidates, and unsupported disposition types; parser, network, verification, catalogue, and discovery failures currently fail the command without producing a partial plan. An advisory entry in later phases labels install/update/removal as `WouldInstall`, `WouldUpdate`, or `WouldRemove` rather than claiming executable authority. Match labels are reasons/evidence; changes are actions; conflicts and failures block execution. Keeping these dimensions separate prevents a misleading state such as treating “Exact match” as permission to overwrite a conflict.

Preview may read destination metadata and hashes when its phase permits but creates no directories, lock files, downloads, caches, backups, manifests, journals, or audit files. Fetching is a separate preceding operation; Phase 1 holds its metadata snapshot in memory. Artifact retrieval and inspection happen before later artifact-backed previews in a private manager staging area. Immediately before execution, all plan preconditions—source version and metadata hash, artifact hash, adapter/version, installation and root identities, normalized destination set, ownership state, existing hashes, match evidence, and policy version—are revalidated under the installation lock. A stale plan is rejected and regenerated; approval never transfers to the regenerated plan.

Plan IDs are hashes of canonical ordered plan inputs and entries, excluding `generated_at` and presentation-only text. Maps and entries are sorted before hashing. The source snapshot captures the approved endpoint and verification policy, and the catalogue snapshot/version is a precondition. This makes determinism testable rather than dependent on clock time or hash-map iteration.

## Manifest, Ownership, Backup, and Rollback

The manager initially uses versioned, manager-owned manifest files and journals rather than changing the existing database schema.

```rust
pub struct InstallManifest {
    pub schema_version: ManifestSchemaVersion,
    pub manifest_id: ManifestId,
    pub previous_manifest: Option<ManifestRef>,
    pub installation_id: InstallationId,
    pub approved_root_id: ApprovedRootId,
    pub source_id: SourceId,
    pub source_version: SourceVersion,
    pub adapter_id: AdapterId,
    pub game_identity: Option<GameIdentitySnapshot>,
    pub downloaded_artifact_hash: ContentHash,
    pub installed_at: Timestamp,
    pub updated_at: Timestamp,
    pub status: InstallStatus,
    pub rollback_state: RollbackState,
    pub files: Vec<InstalledFileRecord>,
}

pub struct InstalledFileRecord {
    pub relative_path: SafeRelativePath,
    pub install_disposition: FileInstallDisposition,
    pub installed_hash: ContentHash,
    pub previous_file_backup: Option<BackupRef>,
    pub previous_file_hash: Option<ContentHash>,
    pub installed_metadata: RestorableFileMetadata,
    pub previous_file_metadata: Option<RestorableFileMetadata>,
    pub file_status: InstalledFileStatus,
}
```

The manifest records source ID and immutable source version, emulator adapter, game identity evidence, approved root identity plus every installed relative path, downloaded artifact hash, per-file installed hash, whether each file was created or replaced, previous-file backup reference, restorable metadata, installation and update timestamps, status, and rollback state. It never treats a persisted absolute path as authority. `BackupRef` addresses content by hash inside a private backup store and includes length and metadata needed for safe restoration. Restore support is limited initially to regular-file bytes and a documented safe subset of permissions/timestamps; unsupported ownership, ACL, extended-attribute, sparse-file, or security-label preservation blocks replacement rather than silently losing metadata.

Each update creates an immutable manifest generation linked to its predecessor. “Rollback available” means exactly the predecessor generation and all of its required backups are present and verified; arbitrary multi-version rollback is not implied. Backups cannot be garbage-collected while referenced by any retained manifest generation or incomplete journal. Retention and reference counting operate under the same manager lock and use a mark-from-valid-manifests-and-journals pass before deletion.

Ownership is the tuple `(installation_id, approved_root_id, normalized_relative_path)` plus the recorded installed hash. There may be at most one active manifest owner for that tuple. Before planning and again under lock before mutation, the manifest store builds and validates the ownership index; duplicate owners, case/normalization aliases, an unreadable manifest, or an unsupported manifest version block mutation for the affected installation. A new source cannot adopt or replace another manifest's path without an explicit ownership-transfer workflow, which is not part of the initial release.

Cleanup and rollback may operate only on regular files and backups referenced by a valid manifest. They never scan an emulator directory and infer ownership from an extension or filename. Manager-created directories are recorded separately with root-relative paths and may be removed only if still the same directory, empty, and unclaimed; otherwise they remain. Pre-existing directories are never owned. A file whose current hash or supported metadata differs from the recorded installed state is `LocallyModified`; it is reported and left untouched unless the user explicitly approves that exact replacement or restoration in a fresh plan. Removal candidates are previewed and confirmed using the same rule. Missing files are reported as drift and are not recreated during audit or cleanup.

Manifest and journal records use checksummed, versioned envelopes. Writes use create-new temporary files, restrictive permissions, serialization and self-read validation, file sync, atomic rename, and parent-directory sync. Immutable manifest generations are never replaced; only a checksummed active-generation reference is atomically switched after the new generation is durable. If the platform/filesystem cannot provide the required durability primitive, installation is disabled rather than claiming crash safety. The initial journal is durable before any backup or staging allocation, and backups and staged files are durable before destinations change. The journal records the immutable approved plan hash, the exact user confirmation or unattended-policy decision bound to that hash, old/new manifest hashes, root identity, every expected old/new/backup hash, and a durably published state before and after each rename. Operation IDs and state transitions are idempotent. A committed journal is retained until a later garbage-collection pass; deleting a journal is not the commit point.

Recovery is conservative and runs under the installation lock:

| Observed durable state | Recovery action |
| --- | --- |
| No destination rename occurred | Verify old state, remove only journal-owned staging, and abort the operation. |
| A destination matches the journaled new hash and remaining destinations match their journaled old hashes | The operation is partially applied. Do not expose it as complete. Automatically restore already replaced files, and remove newly created files, only when every required backup verifies and every affected destination still matches an expected old/new hash; otherwise stop for manual review. |
| All destinations match new hashes but the new manifest is not current | Publish the already prepared, hash-verified new manifest only if the old manifest and every precondition still match the journal. No new user approval is inferred. |
| New manifest is current and all destinations match it | Mark the journal committed and later garbage-collect staging according to policy. |
| Any destination, backup, root, journal, or manifest has an unexpected identity/hash | Make no further destination changes, preserve evidence/backups, mark `RecoveryBlocked`, and require explicit review. |

Rollback uses the same rules with old and new generations reversed. Recovery never downloads data, chooses a different source version, regenerates a plan, overwrites a locally modified file, or performs a “best effort” mixture. Doctor reports incomplete/corrupt journals, missing or corrupt backups, manifest/file hash drift, ownership conflicts, orphaned staging objects, inaccessible or replaced targets, unsupported durability, and unsupported manifest versions.

Destination paths are stored as validated relative components bound to an adapter installation identity and approved root, not accepted later as arbitrary strings. An advisory lock coordinates ArchiveFS processes but does not protect against external attackers. For mutation, the transaction layer opens the approved root without following links, records its filesystem identity, walks/creates children relative to held directory handles with no-follow/beneath constraints, permits only regular-file leaves, and renames relative to those handles. It compares the opened objects—not a later canonicalized string—with planned identities immediately before backup and rename. Unsupported platforms/filesystems remain preview-only until equivalent race-resistant primitives are designed and tested.

## Dependency Policy and Doctor

Built-in Rust implementations are preferred for HTTP, hashing, signature verification, serialization, archive parsing, path validation, and supported patch parsing. Adding a crate still requires normal dependency, maintenance, and security review.

Optional external tools are capability providers, never hidden requirements. Each one must:

- be detected by Doctor and adapter health checks;
- pass an explicit version and required-capability check, not merely exist on `PATH`;
- disable only the affected format or operation when missing or incompatible;
- never be installed automatically and never invoke `sudo`;
- be invoked without a shell, using locally generated argument vectors and constrained files;
- never receive arguments or executable paths selected by downloaded metadata;
- run with an explicit working directory, minimal allowlisted environment, closed inherited file descriptors, bounded input/output/time, and captured/redacted diagnostics; sandboxing is required when the platform can provide it and lack of an approved sandbox must be visible;
- have output treated as untrusted and verified against the expected output path, type, size, and hash before use;
- report its availability, version, trust boundary, and disabled features truthfully in both GUI and CLI.

For example, a future xdelta application capability may use a reviewed Rust implementation or a locally configured compatible tool. Its absence must not disable cheat database preview, native adapter formats, auditing, or rollback. “Metadata supported, application unavailable” is preferable to pretending full support.

## Proposed Crate and Module Boundaries

The current three-crate workspace can support the feature without a new crate initially. Proposed modules and public types are:

```text
crates/archivefs-core/src/patch_manager/
  mod.rs                 PatchManager, PatchManagerError, ManagerPolicy
  source.rs              PatchSourceConfig, SourceId, SourceRegistry
  retrieval.rs           SourceClient, MetadataSnapshot, DownloadedArtifact
  verification.rs        ArtifactVerifier, VerificationPolicy, VerificationResult
  inspection.rs          ArtifactInspector, InspectionPolicy, InspectedArtifact
  matching.rs            GameIdentity, GameMatcher, GameMatch, MatchConfidence
  planning.rs            PatchPlanner, AdvisoryPatchPlan, PatchPlan, PlanEntry, PlanAction
  manifest.rs            ManifestStore, InstallManifest, InstalledFileRecord
  backup.rs              BackupStore, BackupRef, BackupRetentionPolicy
  transaction.rs         InstallTransaction, RollbackTransaction, OperationJournal
  audit.rs               AuditEvent, AuditLog, AuditReport
  doctor.rs              PatchManagerDoctor, CapabilityCheck
  adapters/
    mod.rs               EmulatorAdapter, AdapterRegistry, AdapterCapabilities
    retroarch.rs          RetroArchAdapter
    rpcs3.rs              Rpcs3Adapter
    pcsx2.rs              Pcsx2Adapter
    dolphin.rs            DolphinAdapter
    xenia.rs              XeniaAdapter
    custom_folder.rs      CustomFolderAdapter

crates/archivefs-cli/src/patch_manager/
  mod.rs                 command dispatch only
  format.rs              AdvisoryPlanFormatter, PatchPlanFormatter, AuditReportFormatter

crates/archivefs-gui/src/patch_manager/
  mod.rs                 feature-local public surface
  state.rs               PatchManagerState, PatchManagerMessage, async task results
  view.rs                source/preview/audit/rollback rendering
```

`archivefs-core` owns all policy and domain decisions. Retrieval cannot import adapters or manifests. Verification receives bytes/streams and policy, not destination paths. Inspection returns safe relative entries but cannot select destinations. Planning may query adapters and manifests but cannot mutate. Transactions can mutate only through validated plans and stores. Adapters cannot access network clients. CLI formatting and GUI rendering consume shared serializable view models and do not independently decide safety.

The current filesystem layout is still smaller than the target layout above, but now includes both the neutral adapter boundary and RetroArch's own independent preview: `patch_manager/mod.rs` contains source constants, parsing, catalogue evidence, PCSX2-specific orchestration (platform filtering, probable/uncertain heuristics, ambiguity, plan/plan-ID assembly), and advisory planning - unchanged by RetroArch's addition; `patch_manager/adapter.rs` contains the neutral `EmulatorAdapter` trait and its supporting types (`AdapterId`, `AdapterCapabilities`, `InstallationCandidate`, `AdapterIdentityEvidence`, `HypotheticalDestination`, `DiscoveryConfidence`) - also unchanged; `patch_manager/matching.rs` contains the one small neutral exact-identity-evidence comparison function - unused by RetroArch, since it has no external record to compare identity evidence against (see [`docs/RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md)); `patch_manager/retrieval.rs` contains the fixed-endpoint HTTPS boundary, accepting only the PCSX2 URL - not used, imported, or extended by RetroArch's preview, which makes no network call at all; `patch_manager/pcsx2.rs` contains read-only standard-path candidate discovery plus the first (and only) `EmulatorAdapter` implementation, including all PCSX2-only serial/CRC normalization, PNACH filename parsing, and destination calculation; and `patch_manager/retroarch.rs` contains RetroArch's own independent read-only preview - reusing `emulator_environment::retroarch`'s discovery directly, its own `CoreMatchDisposition`/`ProposedDestination`/`RetroArchAdvisoryPlan` types, and its own core-extension-based matching, deliberately *not* implementing `EmulatorAdapter`. This still avoids an `adapters/` subdirectory - two adapters/previews, one of which is not even a trait implementation, do not yet justify that additional layer. The manifest, backup, transaction, audit persistence, artifact inspection, mutation-adapter methods, other emulator modules, GUI state/rendering, and all install/update/remove/rollback CLI formatting remain absent. Listing later modules here reserves boundaries; it does not authorize scaffolding or implementation beyond what is described above.

The GUI should use a feature-local state/update/view boundary when implementation begins, avoiding unrelated changes to the existing main GUI entry point until integration is explicitly scheduled. The CLI should remain a thin projection over core operations. If compilation time or dependency isolation later warrants new workspace crates, `archivefs-patch-core`, `archivefs-patch-net`, and `archivefs-emulator-adapters` are reasonable extraction points, but adding them is not required for phase one.

## Threat Model

| Threat | Required mitigation and failure behavior |
| --- | --- |
| Malicious source server | Treat all responses as hostile; bound transport and parsing; require preview; never execute content; restrict destinations through adapters. |
| Compromised upstream repository | Prefer pinned signatures or authenticated checksums; expose provenance; detect verification downgrade; allow source disable and rollback. |
| Archive bombs | Enforce compressed, expanded, ratio, entry-count, nesting, per-entry, time, and memory limits while streaming; abort safely. |
| Path traversal | Reject absolute paths, prefixes, `..`, normalization ambiguity, collisions, and special entries before destination mapping. |
| Symlink attacks | Reject archive links; use no-follow/open-relative techniques where supported; revalidate destination ancestry immediately before rename. |
| Destination folder replacement | Bind plans to opened root identity; walk beneath held no-follow directory handles; use no-replace/exchange operations; abort stale or changed objects. |
| Checksum downgrade | Persist expected verification strength; loss of a signature/checksum blocks update and requires explicit policy change. |
| Replayed old metadata | Track last accepted source version/hash and authenticated time/counter where available; block observed rollback unless local policy changes; disclose that first-seen replay is undetectable without authenticated freshness. |
| Interrupted installs | Durable backups, same-filesystem staging, operation journal, idempotent recovery, atomic per-file rename, and atomic manifest publication. |
| Manually modified installed files | Compare current and manifest hashes; report drift; leave untouched without explicit replacement approval. |
| Source impersonation | Stable source IDs, pinned origin/key material, strict TLS, redirect credential isolation, and visible origin/provenance. |
| Secrets leakage | Store only credential references; redact URLs/headers/errors; scope credentials to origins; exclude secrets from manifests, logs, exports, and GUI snapshots. |
| Patch matched to wrong game | Evidence-based confidence, contradiction detection, exact-only unattended eligibility, and mandatory review for probable/uncertain matches. |
| SSRF, malicious redirects, and DNS rebinding | Validate scheme and every resolved/redirect target; block local/private/special networks by default; bind credentials to approved origins; recheck the connected peer. |
| Malicious catalogue or local metadata | Treat catalogue strings and identifiers as untrusted; use the same bounded normalization; never convert catalogue paths into destinations or commands. |
| Concurrent ArchiveFS process | Take one installation-namespace lock before revalidation and hold it through journal commit; reject a second operation. |
| Concurrent emulator or benign editor | Detect when reliable, warn/block according to the adapter profile, use atomic exchange/no-replace operations, verify the displaced object, and recover conservatively on drift. |
| Malicious same-user process | Not fully preventable: same-UID code can mutate accessible files and manager state around operations. Restrictive permissions, held directory handles, atomic exchange, hashes, and journals reduce races but do not form a sandbox boundary. |
| Disk full, I/O error, or lost durability | Check capabilities, treat every write/sync as fallible, publish journal states durably, and follow only the recovery table. |
| Corrupt or forged local manifest/journal | Checksummed/versioned envelopes and restrictive permissions detect accidents; invalid state blocks mutation. Without a signing key unavailable to the same user, local authenticity is not guaranteed. |

Residual risks remain. HTTPS without pinned artifact verification cannot protect against a compromised upstream. A malicious endpoint can transmit mislabeled bytes before the client rejects them. Filesystem and platform APIs differ in their resistance to rename and symlink races, and a hostile process running as the same user is outside the enforceable isolation boundary. Emulator formats evolve and may contain semantics that parsers do not understand. A valid patch can still be malicious at runtime inside an emulator even if it is data-only. The UI must communicate these facts rather than present verification as a guarantee of fitness, legality, or safety.

## Phased Implementation

### Phase 0: Domain types, policies, and fixtures

Define only the read-only source-metadata, PCSX2 discovery, identity/matching, and `AdvisoryPatchPlan` types needed by Phase 1. Use in-memory repositories and fixtures only; do not add networking or any write-capable manifest, backup, transaction, mutation-adapter, executable-plan, artifact-download, or payload-parser implementation. The later-phase types in this document remain design sketches until their phase is separately approved.

In the current worktree this domain groundwork and Phase 1 are one uncommitted change set rather than two shipped milestones. The network client was added only for Phase 1; no write-capable later-phase type was added.

Acceptance criteria:

- Metadata snapshots, PCSX2 installation candidates, identity evidence, match results, and advisory entries are representable without GUI/CLI types.
- Advisory plans distinguish hypothetical actions, blocking dispositions, and match evidence, and have no execution/confirmation API.
- The parser contract is version-labelled and rejects incompatible Git-tree shapes. The upstream response does not carry an independent ArchiveFS schema-version field.
- The patch-manager module has no dependency on mount backends, process execution, manifest/backup code, or mutable catalogue APIs.

Tests: built-in source validation; URL/error redaction; trust defaults; advisory plan determinism; incompatible metadata-version rejection; property tests for hypothetical relative-path and PCSX2 identifier normalization; compile-time/API checks that no executor accepts an `AdvisoryPatchPlan`; dependency review that the Phase 0 module cannot obtain mount, process, or write-capable catalogue interfaces.

### Phase 1: Current PCSX2 read-only slice

The implementation supports exactly one provisional, compiled-in HTTPS JSON metadata endpoint and `ReadOnlyPcsx2Adapter`. It fetches one metadata document into bounded memory, hashes it, validates a fixed parser contract labelled `github-git-tree-v1`, reports PCSX2 installation candidates without creating them, reads an existing ArchiveFS catalogue snapshot, and renders an `AdvisoryPatchPlan` through `pcsx2-patch-preview [--json]`. The source definition is not user-editable. The endpoint is the official `PCSX2/pcsx2_patches` repository's Git-tree API, but its use is **not** an approval to redistribute or install repository content; repository-wide patch licensing remains unresolved.

The endpoint actually supplies a tree version SHA and entries containing repository path, object type, object SHA, optional size, and ignored fields such as GitHub object URLs. Phase 1 accepts only flat `patches/*.pnach` blob records, validates 40-character Git object IDs, and extracts a syntactically valid PS2 serial and executable CRC from supported filenames. It does not receive authoritative title, region, patch category, supported PCSX2 version range, authenticated publication time, or license fields from downloaded metadata; fixed source provenance and the unresolved license notice are local labels. Downloaded URLs and all unknown fields are ignored and never followed or rendered as actionable links.

It does not fetch or persist referenced checksums, signatures, artifact URLs, PNACH bodies, ZIP files, scripts, binaries, images, or other payloads. It does not parse PNACH payload grammar, hash or mount game content, inspect emulator cheat or patch directories for installed files, evaluate writability, create source configuration, update `last_successful_update`, schedule refreshes, write cache/audit/config/manifest files, or expose install/update/remove/rollback commands. It opens an existing current-schema catalogue explicitly read-only, performs no migration or schema creation, ignores catalogue rows already marked missing for matching, and reports missing/incompatible catalogue state instead of repairing it.

Implemented limits are: exactly one HTTPS URL; no authentication or proxy; zero redirects; identity encoding only; 3-second DNS, 5-second connect, 5-second response, 10-second body, and 15-second overall deadlines; at most 8 MiB of received identity-encoded metadata; at most 50,000 Git-tree entries; at most 32 JSON nesting levels; and at most 4 KiB for every consumed schema string. Serde rejects duplicate consumed fields, and duplicate accepted patch record paths are rejected; duplicate unknown ignored keys are not claimed to be rejected. The 8 MiB body limit also bounds ignored metadata, but the implementation does not separately count every collection nested solely inside an ignored field. These values may be reduced during implementation review but not raised without updating the design.

Acceptance criteria:

- A user can invoke one preview command that fetches the compiled-in source into memory, displays its endpoint, transport-only or stronger verification state, metadata hash, age supplied by authenticated metadata if any, provenance/license reference, and advisory PCSX2 matches/hypothetical relative destinations.
- Exact/probable rules are unit-tested with synthetic domain evidence, but the current endpoint/catalogue combination can produce only uncertain or no-match game results because approved exact fields and source titles/regions are absent. Missing-game, no-candidate, ambiguous-candidate, and verification provenance are visible.
- Every entry is structurally non-executable; no confidence level can be marked unattended or converted to an executable plan.
- Multiple standard-path PCSX2 candidates remain separate and produce a blocked advisory result; no root is silently selected. Discovery does not establish installation, version, writability, or mutation readiness.
- The patch-manager code makes no filesystem mutations and works when all observed catalogue/emulator paths are mounted read-only. Metadata is discarded on exit. Existing catalogue, scan, and mount behavior remains unchanged and is not invoked as a side effect.
- Phase 1 uses only identity fields already exposed by the approved read-only catalogue interface and adds no schema fields. The current interface exposes no approved PS2 serial/CRC/hash evidence, so production exact matching is blocked; synthetic matcher fixtures do not change that limitation.
- Failure is fail-closed and truthful: network, TLS, redirect, schema, limit, catalogue, and discovery failures produce no partial trusted snapshot and no stale fallback because Phase 1 has no cache.

Current tests cover the fixed-endpoint guard, redirect-status rejection at the fetch boundary, received-size limit, non-identity encoding policy, malformed/truncated/deep metadata, duplicate consumed fields and accepted records, unsafe repository paths, serial/CRC parsing, synthetic exact/probable/uncertain/conflicting/ambiguous evidence, missing catalogue rows, native/Flatpak/final-component-symlink candidates, multiple candidates, no candidate directory creation, read-only current-schema catalogue opening, outdated/missing catalogue refusal, attempted SQL mutation failure, unchanged catalogue bytes/default sidecar set for the exercised fixture, deterministic decision-based plan IDs, hypothetical output, and human/versioned JSON presentation. The filesystem abstraction proves discovery invokes only root metadata probes; the fixture snapshot tests prove the exercised preview path did not change fixture paths or catalogue bytes. They do not prove that arbitrary dependencies lack all write syscalls, that every ancestor is symlink-free, or that SQLite can never interact with pre-existing WAL/SHM state under every external concurrency pattern.

Before describing Phase 1 as release-hardened, add deterministic resolver/connector tests for connected-peer validation and DNS rebinding, a local TLS server with a test CA, certificate rejection, timeout behavior, header and error redaction, and explicit response truncation tests against the actual HTTP stack. Add read-only fixtures for any supported SQLite journal state and run the CLI against permission-enforced read-only trees. These are remaining validation requirements, not guarantees supplied by the current unit tests.

### Phase 2: Safe artifact download and inspection

Download only approved small data formats from the phase-one source into private temporary storage. Add hashing, checksum/signature policy, ZIP or single-file inspection, strict limits, and path validation. Do not install.

Acceptance criteria:

- Every artifact has a computed hash and explicit verification strength.
- Executables, scripts, complete-game-like payloads, traversal entries, links, bombs, excess entries, and oversized files are rejected.
- Verification failure or downgrade produces a visible blocked `VerificationFailed` plan entry and prevents an execution-eligible plan; it does not prevent safe advisory reporting.
- Interrupted downloads leave no trusted cache entry and can be cleaned safely.

Tests: malicious archive corpus covering `../`, absolute/drive/UNC paths, Unicode normalization and case collisions, NUL/reserved names, symlinks/hard links/special files, nested archives, forged sizes, streamed expansion beyond declarations, high compression ratios, too many files, executable/script magic independent of extension, complete-game-like payload limits, checksum mismatch, invalid signature, downgrade, truncation, cancellation, disk-full behavior, and parser fuzzing. Tests assert rejected objects never enter the trusted cache and no destination path is opened.

### Phase 3: Transactional install, manifest, audit, and rollback for PCSX2

Enable installation of a narrowly defined PNACH data file through the transaction API. Add private backups, operation journals, atomic manifest files, audit output, rollback, and Doctor recovery checks.

Acceptance criteria:

- Installation requires a fresh approved plan and confirmation before replacement.
- The manifest records every installed path and hash plus source, adapter, game, artifact, timestamps, status, and rollback state.
- Existing files are durably backed up before replacement; rollback restores only manifest-owned files.
- Locally modified installed files are reported and untouched by update, removal, or rollback unless explicitly approved.
- Injected interruption at every durable state boundary results in one of the explicit recovery-table states; no unexpected state is automatically mutated.

Tests: a model-based state-machine test enumerates every crash point before/after journal, backup, each destination rename, manifest publication, and committed marker for one- and multi-file operations, then checks recovery idempotence and the recovery table. Fault injection includes short writes, failed sync, disk full, corrupt/torn journal and manifest, same/different filesystem behavior, unsupported durability, permissions/metadata preservation, and missing/corrupt backups. Linux integration tests use controlled race hooks around directory-handle walking and rename to attempt symlink swaps, ancestor replacement, mount-point/device changes, case aliases, and concurrent operations; they assert no operation escapes the held approved root and that unsupported primitives disable mutation. Hash-drift and ownership-collision tests assert no automatic write. These tests establish behavior on the tested filesystem/API matrix; they do not claim to prove untested platforms safe.

### Phase 4: CLI surfaces and Doctor capabilities

Extend the minimal Phase 1 preview CLI with separately approved source list/update, artifact-backed preview, install, audit, rollback, and Doctor reporting through thin handlers. External tools remain unnecessary for the supported path. User-added source editing remains a separate approval even in this phase.

Acceptance criteria:

- Human and structured output truthfully show trust, verification, match confidence, pending changes, confirmations, and disabled capabilities.
- Noninteractive execution refuses required confirmation and never promotes probable/uncertain matches.
- Doctor reports source reachability separately from local capability, incomplete journals, manifest drift, and recovery guidance.

Tests: CLI argument parsing; golden human/JSON output; noninteractive refusal; exit-code mapping; redaction; offline operation; missing emulator/source and interrupted-operation reports.

### Phase 5: GUI workflow

Add source management, refresh progress, preview review, explicit approvals, audit, recovery, and rollback views backed by the same core plans.

Acceptance criteria:

- The GUI never labels a plan installed before durable manifest completion.
- Blocking and review-required states cannot be bypassed through navigation or stale async messages.
- Cancellation leaves fetch/inspection safe and install recovery explicit.
- Restart/rescan notices and optional capability limitations are visible.

Tests: state-machine unit tests; stale task/result rejection; confirmation gates; cancellation; crash/restart restoration from journals; rendering snapshots for every required plan state.

### Phase 6: Deferred adapters and signed sources

RetroArch's own read-only cheat/patch destination preview shipped as a separately reviewed phase (`retroarch-patch-preview`; see [`docs/RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md)) - as an independent module, not an `EmulatorAdapter` implementation, since its multi-root and core-selection-ambiguous shape did not fit that trait. Only now consider adapters one at a time for Dolphin, RPCS3, PPSSPP, DuckStation, Xenia, Azahar/Citra-compatible 3DS emulators, Ryujinx-compatible Switch layouts where legally and technically appropriate, MAME, ScummVM, DOSBox variants, and an explicit custom folder. These are all deferred; no empty modules, advertised capabilities, discovery probes, or format claims should be added before their own layout/identity/source review. Add source formats only with format-specific parsers and fixtures. Introduce signing infrastructure only after key lifecycle decisions are approved.

Acceptance criteria:

- Each adapter has an explicit supported-version/format matrix and cannot write outside discovered, approved roots.
- Generic retrieval, verification, inspection, manifest, backup, and transaction tests run unchanged against every adapter.
- A missing optional tool disables only the declared capability and is reported consistently in Doctor, CLI, and GUI.
- No adapter introduces executable downloads, shell construction, automatic tool installation, `sudo`, or original-game mutation.

Tests: adapter contract suite; emulator layout/version fixtures; per-format parser fuzzing; global/per-game mapping; restart/rescan reporting; ambiguity handling; custom-root containment; cross-adapter manifest isolation.

## Auditing and Observability

Persistent audit storage begins only in a later approved phase; Phase 1 emits bounded in-memory diagnostics and CLI output. Later audit events record source ID/version, metadata snapshot hash, optional artifact hash, verification result, adapter/installation ID, match evidence, plan ID, user or unattended-policy decision, operation ID, changed manifest IDs, result, and sanitized error category. They do not record secrets or unnecessary game paths in exported diagnostics. CLI and GUI can derive a chronological report and answer: what source supplied this file, why it matched this game, what bytes were installed, what was replaced, and whether rollback is available.

Logs are diagnostic, not the ownership record. A missing log must not impair rollback; a corrupt or incompatible manifest must block destructive cleanup. Retention policies must distinguish disposable download cache, security/audit history, manifests, and rollback-critical backups.

## Open Questions

### Phase 1 recorded choices and unresolved blockers

The current implementation records these provisional choices: the official `PCSX2/pcsx2_patches` Git-tree API endpoint; standard XDG native and `net.pcsx2.PCSX2` Flatpak directory candidates; parser contract `github-git-tree-v1`; CLI command `pcsx2-patch-preview` with format-versioned JSON; 8 MiB/50,000/32/4 KiB limits; zero redirects; identity encoding only; and visibly labelled `TransportOnly` verification with unauthenticated freshness. These choices authorize metadata-only evaluation, not artifact retrieval or mutation.

Remaining blockers are:

- Confirm that the endpoint may be indexed and displayed under its terms, and determine the licensing/redistribution policy for individual patches before any caching, redistribution, artifact download, or installation.
- Confirm which PCSX2 versions and Linux layouts may graduate from standard-path candidates to validated installations, including custom/portable layouts and configured patch roots.
- Decide whether the GitHub API shape is an acceptable long-term source schema or whether ArchiveFS requires a separately versioned manifest with title, region, category, PCSX2-version, licensing, and authenticated freshness fields.
- Approve authoritative catalogue identity fields. Current catalogue rows have no PS2 serial, executable CRC, or game hash, so production `Exact` matching is blocked.
- Complete the actual HTTP/TLS and read-only filesystem hardening tests listed in Phase 1 before release claims exceed the current fixture evidence.

### Later-phase decisions

- Which upstream cheat and patch sources permit indexing, caching, redistribution, mirroring, or bundling under their licenses and terms?
- Must ArchiveFS distribute only source definitions, or may it redistribute reviewed metadata/artifacts from particular projects?
- What project policy distinguishes a small lawful patch from copyrighted game content, and what size/format heuristics should trigger rejection or legal review?
- After Phase 1, which emulator versions and native formats are supported next, and which format semantics can safely be parsed without launching the emulator?
- Where should manager source configuration, manifests, journals, cache, audit history, and backups live on each supported platform?
- What backup retention default balances reliable rollback with storage use, and how should shared content-addressed backups be reference-counted?
- Which operations, if any, may later be unattended beyond exact-match updates, and how does a user revoke that policy globally?
- Which exact identifiers are authoritative for each emulator, game revision, executable version, disc, and region?
- Should catalogue hashing be opt-in because of cost, and which content hash definitions are stable for compressed versus mounted archives?
- What signing system is appropriate: project-maintained keys, upstream keys, Sigstore-style identity, TUF, or another framework?
- How are key rotation, revocation, expiry, threshold signatures, trust-on-first-use, and offline roots handled?
- How is rollback/replay of a legitimately signed but vulnerable old patch prevented when upstream metadata lacks monotonic versions?
- Which archive formats are accepted, what exact default limits apply, and is nested archive support necessary at all?
- Can platform-specific filesystem APIs provide sufficiently strong no-follow and directory-identity guarantees, and what platforms/filesystems must be supported?
- Are IPS/BPS/xdelta artifacts metadata-only indefinitely, or will a separately sandboxed derived-copy workflow be designed?
- If an optional external patch tool is ever allowed, what exact binary provenance, version range, sandbox, argument, and output-verification requirements apply?
- How should emulator-running detection work without controlling or terminating emulator processes?
- What audit retention/export and privacy expectations apply to game identities, paths, source history, and authentication diagnostics?
- Should future persistence move into a dedicated database after the file-manifest design proves stable, and what migration/recovery guarantees would be required?

## Non-Goals for the First Release

- Downloading or distributing ROMs, ISOs, complete games, firmware, BIOS files, or emulator binaries.
- Editing, repacking, deleting, or replacing original ArchiveFS game archives.
- Executing downloaded scripts, installers, binaries, hooks, or shell fragments.
- Automatically installing dependencies or invoking privileged commands.
- Applying IPS, BPS, or xdelta patches to original games.
- Guessing an emulator installation or game match and silently writing to it.
- Coupling patch management to ArchiveFS mount or unmount execution.
