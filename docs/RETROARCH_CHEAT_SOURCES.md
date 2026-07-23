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
bounded connect, idle-read, and overall-transfer timeouts. At most three redirects
are followed manually; every target must retain the exact approved host,
HTTPS, and default port. URL credentials, localhost, loopback, private,
link-local, unspecified, and other local endpoints are rejected. DNS answers
are checked before every request. Since the HTTP stack resolves again while
connecting, this preflight reduces but cannot cryptographically eliminate DNS
rebinding; exact host binding and TLS hostname verification remain controls.

No request carries credentials, uploads, telemetry, game metadata, filenames,
or locally computed hashes. Headers are limited to 32 KiB. Content-Length is only an early check: actual
bytes are counted and stopped at the lower of the registry's 256 MiB maximum and
`--max-download-bytes`. Compressed HTTP transfer encoding is rejected. Missing
Content-Length is accepted; a mismatching declared length is rejected.
Accepted bytes stream in 64 KiB chunks directly into an exclusively created
attempt-specific staging file. ArchiveFS updates SHA-256 incrementally and
never accumulates the ZIP as an in-memory response. Progress is emitted at
most once per 2 MiB plus phase transitions, with at most 512 events delivered.

ZIP is the only archive type. Content magic and structure are checked.
Extraction refuses absolute, `.`/`..`, empty-component, Windows drive,
UNC/backslash, NUL, oversized/deep, duplicate, case-fold-colliding, symlink,
hard-link, device, FIFO, socket, and other special entries. Files use
no-overwrite creation beneath symlink-checked staging. Limits are 60,000
entries, 256 MiB per extracted file, 1 GiB total expanded, 1,024 path bytes, 24
components, and 250:1 compression ratio. The per-file extraction limit was
raised because the complete official immutable repository contains larger
non-cheat data files, including `Mobile - J2ME.dat`, observed at 82,656,891
bytes. Those files remain part of the verified manifest even though only `.cht`
files enter cheat matching. Any individual entry above 256 MiB still fails
closed. Nested archives remain inert files and are never recursively extracted.

The compressed archive download limit remains 256 MiB and the total expanded
content limit remains 1 GiB. The revision response limit is 64 KiB,
the serialized manifest limit is 16 MiB, and redirects are limited to three.
Connection establishment (including DNS and TLS) is bounded at 30 seconds,
each response-body read may be idle for at most 60 seconds, and the complete
revision-plus-archive transfer is bounded at 15 minutes. The HTTP request's
global timeout is reduced to the remaining operation time before each request.
One exclusive cache-root lock permits only one source operation at a time.

Transient connection interruption, idle/read timeout, temporary DNS failure,
and HTTP 408, 429, 500, 502, 503, or 504 are retried up to three total attempts.
The ordinary delay is five seconds. Integer-seconds `Retry-After` is honored up
to 30 seconds; unsupported HTTP-date forms use the ordinary delay. TLS/policy,
redirect, size, digest, path, extraction, manifest, and schema failures are not
retried.

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

Each retry starts with a fresh `.part` file; the failed attempt is removed
before waiting or reconnecting. The successful file is flushed and synced
before digest comparison or ZIP processing. Downloads are written in unique
operation staging. Extracted content must
produce a non-empty, structurally complete result from the RetroArch catalogue
parser. The content-addressed snapshot is published before `metadata.json` is
atomically replaced, so failure cannot replace the last known-good snapshot.
Malformed or unsupported individual cheat files are retained in the verified
snapshot but excluded from its derived matching index.

Per-snapshot manifests retain provenance even after a newer snapshot becomes
current; inspection accepts either a source ID or an exact snapshot directory.
Manifest schema 3 records provider ID, canonical repository, immutable archive
URL, exact commit, retrieval timestamp, selected non-sensitive response
metadata, actual size, SHA-256, catalogue/cheat counts, platforms,
completeness, warnings, and a sorted per-file relative path, size, and SHA-256.
It also records total candidate files, indexed files, typed exclusion counts,
and at most 32 deterministic representative exclusions. Released schema-1 and
schema-2 snapshots remain readable and verifiable; their unavailable
repository and exact-revision fields remain empty rather than being invented.
Atomic `metadata.json` records the current snapshot, last successful state,
and timestamped typed refresh failure. Snapshots are
treated as immutable. Inspection does not repair metadata or update times.

A snapshot is fresh for 24 hours. Normal fetch reuses a fresh snapshot and
explicit update refreshes stale data. Cancellation is checked before connect,
after headers, between chunks, before/during retry waits, before extraction,
and before activation. Current-process partial files are removed. A crash can
leave an inactive operation directory; the next provider operation, while
holding the exclusive cache lock, removes at most 16 entries whose names and
types prove they are inactive ArchiveFS staging directories. Unexpected entries
fail closed. The current metadata pointer and previous valid snapshot are
unchanged. `--force-refresh`
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

## Fatal failures and usable partial indexes

`validation_complete` describes the immutable snapshot structure and integrity;
it is not a claim that every third-party `.cht` file can be indexed. Fatal
failures include a missing, invalid, or unsupported manifest; incomplete
download or extraction; archive or file digest mismatch; unsafe, duplicate, or
case-colliding paths; symlinks or special files; a missing catalogue root;
snapshot/metadata binding failures; source identity changes; and any resource
limit that prevents complete verification. These states never materialize a
source.

After structural verification, content indexing has three states: Complete,
Usable partial, and Incomplete. A bounded individual malformed `.cht`, invalid
content encoding, or unsupported path encoding becomes a typed exclusion. It
does not make the structurally verified snapshot incomplete. At most 2,048
excluded identities and 2,048 structural diagnostics are retained; exceeding
either bound fails closed. Non-UTF-8 paths are counted and shown only through a
lossy-marked safe display or count-only text, never reconstructed or invented.

Excluded entries never enter the match candidate list and are never
materialized. Valid entries retain their exact snapshot ID, path, and SHA-256
binding. A diagnostic-only exact title/platform lookup can report `Matching
catalogue entry excluded` for a selected file; otherwise a genuine absence is
reported as `No matching cheat found`. Candidate, weak, and ambiguous matches
remain blocked. Snapshot age is an update hint, not an integrity failure, so a
stale but fully verified immutable snapshot remains usable while Update stays
explicit.

## Sources GUI and Cheats & Mods

The Sources page owns network retrieval. It shows Missing, Ready, Verified with
warnings, Stale,
Invalid manifest, Incomplete, Unsupported schema, Verification failed,
Retrieval failed, Cancelled, and Resource limit reached states. Download is
shown only without a snapshot; Update is shown when local state exists; Verify
is always read-only. Download or Update first opens a review dialog containing
the provider and ArchiveFS-managed destination. Network access begins only
after `Confirm retrieval`. Closing that dialog writes nothing.

During retrieval the Sources page offers cancellation and shows revision,
connection, byte/percentage, retry, archive verification, extraction, file
verification, and activation phases. Unknown Content-Length shows received
bytes without inventing a percentage. The active snapshot is
not changed until revision resolution, archive retrieval, extraction, parsing,
manifest construction, and per-file verification all succeed. Updating does
not modify RetroArch files. Catalogue retrieval activity is session source
history, never an apply journal.

If Update fails, Sources separately shows the latest typed update failure and
the retained active revision, verification time, active file count, indexed
count, and excluded count. Verify performs only local manifest and per-file
digest checks and never opens a network transport.

After activation, an existing Cheats & Mods workspace is refreshed against the
new snapshot while archive selection, Library filters, Recently Found, queue,
mounts, platform assignments, History, and unrelated emulator state remain
unchanged. Source-dependent preview and confirmation state is invalidated.
Cheats & Mods displays the exact upstream revision and differentiates `No
matching cheat found`, `Matching catalogue entry excluded`, verified-with-
warnings, and catalogue or identity failures. The obsolete claim that
RetroArch matching and installation are not implemented has been removed; the
page uses the existing shared preview, controlled apply, History, and rollback
pipeline. Candidate, weak,
ambiguous, PCSX2, and Dolphin entries do not become writable through this
manager.

The canonical Libretro `Sega - Mega Drive - Genesis` and `Nintendo - Super
Nintendo Entertainment System` directories map to ArchiveFS's existing
MegaDrive and SNES platform identities. Valid entries for both systems remain
matchable when unrelated catalogue files are excluded.

For a catalogued loose Mega Drive ROM, shared identity hashes the complete
on-disk file (up to 64 MiB) after validating an exact scanner/manual platform
assignment, a supported extension, a regular no-follow file, and stable file
metadata. That SHA-256 identifies the local bytes only; it is not a known-good
dump or safety claim. The digest is bound into trusted-catalogue preview and is
revalidated before apply. `.smd` bytes are hashed exactly as stored without
header stripping or deinterleaving. See `LOOSE_ROM_CHEAT_SUPPORT.md`.

Existing-local-cheat inspection is independent from the trusted snapshot. For
one selected game it checks only reviewed exact platform-directory aliases and
their immediate `.cht` files. It never recursively walks the full RetroArch
tree. Local results remain unverified and are reported as Not found, filename
Candidate, Exact local filename, Ambiguous, Unsafe, Unavailable, or Limit
reached.

No automatic download occurs at startup, during library scan, during preview,
or during Apply. The Cheats & Mods page links to Sources for Download/Update;
its only direct catalogue action is read-only cached-snapshot reuse.

## Retention and manual Saltbox/Nobara QA

Activation never deletes snapshots. The active snapshot and all previous valid
content-addressed snapshots remain until an explicit cache-maintenance plan is
reviewed and applied; journal/pin protections remain authoritative. This is
more conservative than the minimum retention of one previous valid snapshot.
Failed staging is removed by its operation-scoped cleanup. Only one staging
file and one source operation may be active per provider.

Manual Saltbox checks for the streaming downloader:

1. Open Sources with the current verified snapshot active.
2. Start Update.
3. Confirm visible byte progress.
4. Confirm the approximately 178 MiB catalogue completes.
5. Confirm the exact revision and verification result.
6. Confirm the previous snapshot remains active until activation.
7. Disconnect networking during the transfer.
8. Confirm retry state and attempt number are visible.
9. Restore networking and confirm a later attempt can succeed.
10. Cancel during download and confirm partial attempt files are removed.
11. Confirm the previous snapshot remains usable after cancellation.
12. Confirm Alien 3 matching remains available throughout failed updates.
13. Restart ArchiveFS and confirm no incomplete snapshot is active.
14. Confirm no curl, wget, or other external process is launched.

Current limitations are one reviewed source, ZIP only, integer-seconds
`Retry-After` only, no resumable range download, no standalone
update-availability probe (Update performs the explicit resolve/download), no
user-defined URLs, no automatic pruning, and DNS preflight rather than
connection-pinned resolution. Maintenance never runs implicitly during fetch.
