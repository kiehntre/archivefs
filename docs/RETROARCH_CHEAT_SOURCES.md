# Trusted RetroArch cheat sources

Trusted-source inspection, offline reuse, retrieval, and publication use the
same exclusive cache-root lock as snapshot maintenance. See
[`RETROARCH_CHEAT_CACHE_LOCKING.md`](RETROARCH_CHEAT_CACHE_LOCKING.md) for the
timeout, platform, path-identity, and lock-ordering guarantees.

ArchiveFS retrieves reviewed remote cheat catalogues without giving the
installer network access:

`explicit user confirmation → exact upstream commit resolution → immutable HTTPS archive → bounded download → hash/ZIP validation → safe extraction → strict local catalogue validation → immutable snapshot → separate setup/install flow`

Fetching never installs cheats. Listing and inspection never access the
network or mutate the cache.

The built-in adapter is **Trusted** in ArchiveFS's three-state source model:
its provenance, format, host and limits were reviewed. That status does not
claim that structural inspection proves content malware-free. Local and
community sources are **Unverified**, not malicious by definition, and a
general import scanner for them is not implemented. Concrete unsafe structure
is **Blocked**. See [`CHEATS_MODS_SAFETY.md`](CHEATS_MODS_SAFETY.md) for the
trust, local-only inspection, unknown-code, consent and original-file policy.

## Registry and commands

The compiled registry enables `libretro-buildbot-cheats` for compatibility,
but its authoritative provider is now the official
`https://github.com/libretro/libretro-database` repository. The provider's
moving `master` reference is resolved through the GitHub commits API to an
exact 40-character commit ID. ArchiveFS then downloads only the immutable
`codeload.github.com/libretro/libretro-database/zip/<commit>` archive. The
manifest records the canonical repository, resolver endpoint, exact commit,
immutable archive URL, and archive SHA-256; a branch name alone is never an
installed snapshot identity. Commands accept source IDs, never arbitrary or
environment-provided URLs, and never scrape or follow content links. A future
source requires review of ownership, licence, endpoint, layout, limits, and
reproducibility.

```text
archivefs retroarch-cheat-source-list [--cache-root <path>] [--json]
archivefs retroarch-cheat-source-fetch <source-id> [--force-refresh] [--offline]
    [--expected-sha256 <hash>] [--cache-root <path>]
    [--max-download-bytes <bytes>] [--json]
archivefs retroarch-cheat-source-inspect <source-id|snapshot-path> [--cache-root <path>] [--json]
archivefs retroarch-cheat-setup --source <source-id> [retrieval/setup options]
archivefs retroarch-cheat-setup <catalogue-path> [setup options]
```

`--source` and a local positional catalogue are mutually exclusive. Retrieval
options are rejected for local paths. JSON never prompts, emits a versioned
lower-snake-case result on stdout, and keeps prose diagnostics off stdout.

```text
archivefs retroarch-cheat-source-list
archivefs retroarch-cheat-source-fetch libretro-buildbot-cheats --json
archivefs retroarch-cheat-source-inspect libretro-buildbot-cheats
archivefs retroarch-cheat-setup --source libretro-buildbot-cheats --dry-run
archivefs retroarch-cheat-setup --source libretro-buildbot-cheats --offline --yes
```

Setup shows source name/ID/URL, exact commit, fetch time, archive SHA-256, validation state,
network/cache outcome, staleness, warnings, and immutable path. It preserves
all existing match, confirmation, backup, journal, history, inspection, and
rollback rules.

Fetching or inspecting a source never modifies RetroArch, installs catalogue
entries, or duplicates files in RetroArch's cheat directory. Installation is a
separate explicit workflow with destination conflict, backup and journal
rules. ArchiveFS does not execute catalogue content.

## Network and extraction protections

Production uses certificate-validated HTTPS GET requests only, disabled proxies, identity
transfer encoding, a dedicated user agent, zero automatic redirects, and
bounded DNS/connect/response/body/overall timeouts. At most three redirects
are followed manually; every target must retain the exact approved host,
HTTPS, and default port. URL credentials, localhost, loopback, private,
link-local, unspecified, and other local endpoints are rejected. DNS answers
are checked before every request. Since the HTTP stack resolves again while
connecting, this preflight reduces but cannot cryptographically eliminate DNS
rebinding; exact host binding and TLS hostname verification remain controls.

No request carries credentials, uploads, telemetry, game metadata, filenames,
or locally computed hashes. Headers are limited to 32 KiB. Content-Length is only an early check: actual
bytes are counted and stopped at the lower of the registry's 64 MiB maximum and
`--max-download-bytes`. Compressed HTTP transfer encoding is rejected. Missing
Content-Length is accepted; a mismatching declared length is rejected.
Accepted bytes stream directly into a unique staging file; they are never
accumulated as an unbounded in-memory response.

ZIP is the only archive type. Content magic and structure are checked.
Extraction refuses absolute, `.`/`..`, empty-component, Windows drive,
UNC/backslash, NUL, oversized/deep, duplicate, case-fold-colliding, symlink,
hard-link, device, FIFO, socket, and other special entries. Files use
no-overwrite creation beneath symlink-checked staging. Limits are 60,000
entries, 8 MiB per file (the shared local catalogue bound), 256 MiB total expanded, 1,024 path bytes, 24
components, and 250:1 compression ratio. Nested archives remain inert files
and are never recursively extracted.

The archive download limit is 64 MiB, the revision response limit is 64 KiB,
the serialized manifest limit is 16 MiB, redirects are limited to three, and
the global request timeout is 45 seconds. One exclusive cache-root lock permits
only one source operation at a time.

## Cache, provenance, offline mode

The default cache is `~/.local/share/archivefs/cheat-sources/`; `--cache-root`
overrides it explicitly:

```text
<cache-root>/<source-id>/
  snapshots/<archive-sha256>/<catalogue-prefix>/...
  manifests/<archive-sha256>.json
  metadata.json
  .staging/...
```

Downloads are written and synced in unique staging. Extracted content must
produce a non-empty, complete result from the existing RetroArch catalogue
parser. The content-addressed snapshot is published before `metadata.json` is
atomically replaced, so failure cannot replace the last known-good snapshot.
Malformed individual cheat files are retained only as reported,
non-actionable entries rather than silently weakening validation.

Per-snapshot manifests retain provenance even after a newer snapshot becomes
current; inspection accepts either a source ID or an exact snapshot directory.
Manifest schema 2 records provider ID, canonical repository, immutable archive
URL, exact commit, retrieval timestamp, selected non-sensitive response
metadata, actual size, SHA-256, catalogue/cheat counts, platforms,
completeness, warnings, and a sorted per-file relative path, size, and SHA-256.
Released schema-1 snapshots remain readable and verifiable; their unavailable
repository and exact-revision fields remain empty rather than being invented.
Atomic `metadata.json` records the current snapshot, last successful state,
and timestamped typed refresh failure. Snapshots are
treated as immutable. Inspection does not repair metadata or update times.

A snapshot is fresh for 24 hours. Normal fetch reuses a fresh snapshot and
explicit update refreshes stale data. Cancellation is checked before activation;
an inactive staged/content-addressed directory may remain, but the current
metadata pointer and previous valid snapshot are unchanged. `--force-refresh`
retains the previous snapshot until a replacement validates. `--offline` makes no network call, reports stale reuse,
and fails without a valid current snapshot. No automatic deletion policy is
implemented; content hashes deduplicate identical fetches, while deliberate
preview-first cleanup and external pins are documented in
[`RETROARCH_CHEAT_CACHE_MAINTENANCE.md`](RETROARCH_CHEAT_CACHE_MAINTENANCE.md).

Every successful online retrieval is pinned to the resolved commit and records
its computed archive SHA-256. An independently obtained expected digest may
still be supplied through the CLI. Use source inspection for freshness,
provenance, usability, last outcome, and stage-specific registry/network,
download, validation, extraction, cache, cancellation, or offline errors.

## Sources GUI and Cheats & Mods

The Sources page owns network retrieval. It shows Missing, Ready, Stale,
Invalid manifest, Incomplete, Unsupported schema, Verification failed,
Retrieval failed, Cancelled, and Resource limit reached states. Download is
shown only without a snapshot; Update is shown when local state exists; Verify
is always read-only. Download or Update first opens a review dialog containing
the provider and ArchiveFS-managed destination. Network access begins only
after `Confirm retrieval`. Closing that dialog writes nothing.

During retrieval the Sources page offers cancellation. The active snapshot is
not changed until revision resolution, archive retrieval, extraction, parsing,
manifest construction, and per-file verification all succeed. Updating does
not modify RetroArch files. Catalogue retrieval activity is session source
history, never an apply journal.

After activation, an existing Cheats & Mods workspace is refreshed against the
new snapshot while archive selection, Library filters, Recently Found, queue,
mounts, platform assignments, History, and unrelated emulator state remain
unchanged. Source-dependent preview and confirmation state is invalidated.
Cheats & Mods displays the exact upstream revision and differentiates `No
matching cheat found` from catalogue or identity failures. Candidate, weak,
ambiguous, PCSX2, and Dolphin entries do not become writable through this
manager.

No automatic download occurs at startup, during library scan, during preview,
or during Apply. The Cheats & Mods page links to Sources for Download/Update;
its only direct catalogue action is read-only cached-snapshot reuse.

## Retention and manual Nobara QA

Activation never deletes snapshots. The active snapshot and all previous valid
content-addressed snapshots remain until an explicit cache-maintenance plan is
reviewed and applied; journal/pin protections remain authoritative. This is
more conservative than the minimum retention of one previous valid snapshot.
Failed staging is removed by its operation-scoped cleanup when proven inactive.

Manual Nobara checks:

1. Open Sources and confirm the official Libretro provider card appears without network activity.
2. Confirm Missing shows Download, while Ready or Stale shows Update and Verify.
3. Click Download/Update, review provider and managed destination, then cancel; confirm no network operation or active-snapshot change.
4. Confirm retrieval and observe responsive progress; optionally cancel and verify the previous snapshot remains active.
5. Complete retrieval and confirm an exact 40-character revision, file count, total size, verification state, and successful-update time.
6. Open details and confirm the canonical repository, resolver, immutable archive template, and snapshot SHA-256.
7. Return to an `Alien 3 (USA, Europe).md` Mega Drive selection in Cheats & Mods and confirm the active revision and matching cheat count.
8. Select a synthetic game with no catalogue entry and confirm `No matching cheat found`, with no Apply action.
9. Confirm weak or ambiguous matches remain blocked and PCSX2/Dolphin remain preview-only.
10. Confirm queue, mounts, selected game, Library filters, Recently Found, platform assignment, activity, and transaction History remain intact.
11. Disconnect networking, request Update, and confirm the failure is typed and the previous valid snapshot remains usable.
12. Restart ArchiveFS and confirm the active snapshot status and revision persist without an automatic update check.

Current limitations are one reviewed source, ZIP only, no standalone
update-availability probe (Update performs the explicit resolve/download), no
user-defined URLs, no automatic pruning, and DNS preflight rather than
connection-pinned resolution. Maintenance never runs implicitly during fetch.
