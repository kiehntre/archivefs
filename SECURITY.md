# Security Policy

EmuWiz is alpha-stage software. This document describes its current
safety boundaries and how to report a vulnerability - it is not a security
guarantee or a certification.

## Reporting a vulnerability

If you find a security issue, please open a GitHub issue on this repository
describing the problem, or contact the maintainer directly if the issue is
sensitive enough that you would rather not describe it publicly first.
There is no dedicated security mailing list or bug-bounty program at this
stage, and GitHub private vulnerability reporting is not currently enabled
for this repository.

Please include:

- The affected version or commit.
- Steps to reproduce, or the specific code path you're concerned about.
- What you'd expect to happen instead.

There is currently no formal disclosure timeline commitment, given this is a
small, alpha-stage project - but reports will be looked at and, where
confirmed, fixed and noted in `CHANGELOG.md` under a `Security` entry.
Dependency security updates (Dependabot) and secret scanning, including
push protection, are enabled on this repository.

## Current safety boundaries

These are the security-relevant properties EmuWiz's design and tests
currently rely on. See [`docs/security.md`](docs/security.md) for the fuller
architecture-level detail behind each of these, including the RomM
endpoint policy and token-file handling summarized below.

- **Local-first, no telemetry.** EmuWiz does not send usage data, crash
  reports, or file information anywhere. Outbound network use is opt-in and
  explicit. Public catalogue, cheat, and provider retrievals - the PCSX2
  patch-metadata fetch to a single compiled-in endpoint, and the RetroArch /
  Dolphin / Xenia / GameHacking / BSFree metadata and cheat-catalogue
  fetches - use HTTPS-only transport where the current code enforces that.
  The RomM client is different: RomM is a separately configured local or
  private service, and its endpoint may use HTTP or HTTPS only where the
  endpoint/address policy permits approved loopback or private-LAN
  destinations. Every fetch enforces timeouts, response-size limits, and
  downloaded-content validation; see the network and provider trust notes
  below.
- **Archives are treated as untrusted input.** Filenames and archive
  contents may be attacker-controlled; mount-name generation and path
  handling are designed not to let an archive's name or internal paths
  escape the configured mount area. Encrypted archives and encrypted
  archive entries are refused outright rather than inspected or guessed,
  and archive inspection is bounded by entry/member limits. DAT
  verification of individual ZIP members, NES header normalization, and
  CHD verification are designed but **not yet implemented**; the audit
  currently hashes each outer file as-is.
- **Verification is read-only.** Scanning, mounting, cataloguing,
  duplicate detection, DAT verification, and library-view/patch-preview
  operations all read archive metadata and filesystem state; none of them
  rewrite a source archive file, and no verification path extracts or
  writes an archive member to disk.
- **Mounts and unmounts are scoped.** Mounts are only created under the
  configured `mount_root`; unmounts only ever target paths under it.
  Library View destinations may not overlap a configured source folder.
- **Mutations are journaled and gated.** DAT rename apply and canonical
  ROM organisation move files (or symlink objects) through a single gated
  engine: a durable journal is written **before** any mutation, each entry
  is re-preflighted immediately before its rename, the rename uses an
  atomic no-clobber primitive, and interrupted transactions are reconciled
  or rolled back. Plans carry a generation, and rename/organisation apply
  refuses a plan whose generation is stale or whose classifier version is
  missing or differs from the current rules - before any journal write or
  filesystem mutation. Emulator cheat/patch installs write emulator-owned
  config or data only after an explicit preview and confirmation.
- **The persistent catalogue is additive, not authoritative for safety.**
  Mount, unmount, lazy-unmount, and cleanup code paths read live filesystem
  and mount state directly and do not depend on the SQLite catalogue being
  present, complete, or uncorrupted. See
  [ADR 0001](docs/adr/0001-persistent-library-database.md).
- **Install and update ownership.** The installer records every path it
  writes (the `emuwiz-cli` and `emuwiz` binaries, the legacy
  `archivefs-cli` / `emuwiz-gui` / `archivefs-gui` alias symlinks, the
  desktop entry, and the hicolor icons) in a user-scoped, versioned
  manifest at `$XDG_DATA_HOME/emuwiz-installer/manifest`. Ownership of a
  file slot is proven by the SHA-256 digest of its exact byte content (not
  its name, timestamp, or inode); symlink slots are proven by their exact
  target. The manifest uses a fixed, closed set of slot names - no path is
  ever parsed out of it - and a fail-closed parser. A foreign file,
  symlink, or directory occupying a destination is never overwritten or
  deleted: install leaves it untouched with a warning (and exits non-zero),
  or moves it aside into a freshly and securely created backup directory
  with `--replace-foreign`. Uninstall removes only demonstrably-owned
  assets and never touches a foreign path. The enforced trust boundary is
  the final `emuwiz-installer` bookkeeping path component, which must be a
  real directory; `XDG_DATA_HOME` itself is trusted as it already is for
  every other user data path. Note that install/uninstall do not lock
  against a concurrent same-user process; see the limitations below.
- **Network and provider trust.** Public catalogue, cheat, and provider
  transports are HTTPS-only where the current code enforces that (the PCSX2
  metadata fetcher accepts only its compiled-in endpoint and refuses every
  other URL before networking). RomM is a separately configured local or
  private service: its endpoint policy allows HTTP or HTTPS, but only to
  addresses approved as loopback or private LAN (RFC 1918 / IPv6
  unique-local); the client refuses redirects, or validates them against the
  same policy before any follow, caps response size, and limits artwork
  fetches to RomM's own thumbnail URLs, never arbitrary scraper URLs.
  Because RomM may be configured over HTTP, its bearer token is **not**
  protected by TLS in transit in that case. RomM tokens are supplied by the
  user as a token file: EmuWiz validates restrictive permissions (a token
  file readable by others is refused), stores only the token-file path or
  configuration reference rather than the value, and redacts token values
  from serialization, diagnostics, and logging; it does not create or persist
  a token file itself. Downloaded cheat archives are size- and
  entry-bounded, optionally hash-checked, safely extracted into a staging
  area, validated with the local catalogue parser, and atomically published.
  Provider content is trusted at the transport boundary (HTTPS for public
  retrievals; the approved local/private endpoint for RomM) plus structural
  validation; it is not cryptographically attested as safe.

## Security-sensitive areas for contributors

If you're changing code in these areas, please be especially careful and
call out the safety implications explicitly in your pull request:

- Path validation and mount-name generation (`archivefs-core`).
- Mount/unmount/cleanup execution and lazy-unmount recovery.
- Source-folder and Library-View destination overlap validation.
- Anything that constructs a filesystem path from archive-supplied or
  remote-supplied data.
- The installer's ownership manifest, parser, backup, or uninstall paths.
- The rename/organisation journal, rollback, and generation or
  classifier-version gates.
- Token storage, redaction, or any new outbound network request, new write
  path, or new place where downloaded content is trusted.

## Out of scope

EmuWiz does not currently implement, and reports about the *absence* of
these are expected rather than actionable bugs:

- Malware/virus scanning of archive contents.
- A sandboxed or containerized execution model.
- A GUI-specific permission model beyond the same checks the CLI uses.
- Protection against a same-user adversarial process racing the installer
  or mutation engine (install/uninstall take no lock against concurrent
  processes).
- Integrity guarantees for upstream provider content beyond the transport
  boundary and structural validation: a compromised provider could deliver
  mislabeled data, which EmuWiz bounds and validates but cannot certify.
- Safe execution of EmuWiz as root or via sudo: the installer never uses
  sudo or any system-wide install path.
