# Trusted RetroArch cheat sources

ArchiveFS retrieves reviewed remote cheat catalogues without giving the
installer network access:

`trusted HTTPS source → bounded download → hash/ZIP validation → safe extraction → strict local catalogue validation → immutable snapshot → existing setup/install flow`

Fetching never installs cheats. Listing and inspection never access the
network or mutate the cache.

## Registry and commands

The compiled registry initially enables `libretro-buildbot-cheats`, the
purpose-built rolling RetroArch cheat archive from `buildbot.libretro.com`.
The record fixes its HTTPS URL, permitted host, ZIP type, size, catalogue directory,
provenance, and licence URL. Commands accept source IDs, never arbitrary or
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

Setup shows source name/ID/URL, fetch time, archive SHA-256, validation state,
network/cache outcome, staleness, warnings, and immutable path. It preserves
all existing match, confirmation, backup, journal, history, inspection, and
rollback rules.

## Network and extraction protections

Production uses certificate-validated HTTPS, disabled proxies, identity
transfer encoding, a dedicated user agent, zero automatic redirects, and
bounded DNS/connect/response/body/overall timeouts. At most three redirects
are followed manually; every target must retain the exact approved host,
HTTPS, and default port. URL credentials, localhost, loopback, private,
link-local, unspecified, and other local endpoints are rejected. DNS answers
are checked before every request. Since the HTTP stack resolves again while
connecting, this preflight reduces but cannot cryptographically eliminate DNS
rebinding; exact host binding and TLS hostname verification remain controls.

Headers are limited to 32 KiB. Content-Length is only an early check: actual
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
Metadata records URL/version, timestamp, selected non-sensitive response
metadata, actual size, SHA-256, catalogue/cheat counts, platforms,
completeness, warnings, current snapshot, and last refresh error. Snapshots are
treated as immutable. Inspection does not repair metadata or update times.

A snapshot is fresh for 24 hours. Normal fetch reuses a fresh snapshot and
refreshes stale data. `--force-refresh` retains the previous snapshot until a
replacement validates. `--offline` makes no network call, reports stale reuse,
and fails without a valid current snapshot. No automatic deletion policy is
implemented; content hashes deduplicate identical fetches, while deliberate
preview-first cleanup and external pins are documented in
[`RETROARCH_CHEAT_CACHE_MAINTENANCE.md`](RETROARCH_CHEAT_CACHE_MAINTENANCE.md).

The rolling built-in archive has no compiled-in archive digest or pinned
version. Supply an
independently obtained digest with `--expected-sha256`; computed SHA-256 is
always recorded and displayed. Use source inspection for freshness,
provenance, usability, last outcome, and stage-specific registry/network,
download, validation, extraction, cache, or offline errors.

Current limitations are one reviewed source, ZIP only, synchronous fetching,
no user-defined URLs, no automatic pruning, and DNS preflight rather than
connection-pinned resolution. Maintenance never runs implicitly during fetch.
