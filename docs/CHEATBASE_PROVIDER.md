# CheatBase Provider Stage 1

ArchiveFS supports the historical [CheatBase/CheatBase](https://github.com/CheatBase/CheatBase)
SQLite catalogue as an optional, browse-only third-party source. It is not
bundled with ArchiveFS and no CheatBase action installs or converts a code.

## Verified source

The supported snapshot is the `cheatbase.sqlite` blob at upstream commit
`5894b60d58804d66e15c6b86b062b72d32163391`:

- size: `67,366,912` bytes
- SHA-256: `917f16ce55afa1a21cfdd106239cbaa65317cef21e47152b6471eb5c017a76e3`
- SQLite header: `SQLite format 3`
- source schema: seven application tables, seven maintenance triggers, no indexes
- rows: 43 systems, 39 regions, 51,748 ROMs, 53,877 releases, 24
  devices, 45 categories, and 107,940 cheats

All cheat rows in this snapshot belong to Nintendo DS. Other systems remain
useful for ROM/release identity lookup but do not expose browseable cheats.

**Cheat coverage: Nintendo DS only. Identity metadata: multiple systems.**
The populated device format is Action Replay DS. A non-Nintendo-DS result is
therefore identity metadata only, has no cheat count, and never offers cheat
browsing or installation.

### Verified schema and data quality

The complete application schema is:

- `SYSTEMS`: `systemID` (PK), `systemName`, `systemShortName`,
  `systemHeaderSizeBytes`, `systemHashless`, `systemHeader`, `systemSerial`,
  `systemOEID`, `lastModified`.
- `REGIONS`: `regionID` (PK), `regionName`, `lastModified`.
- `ROMS`: `romID` (PK), `systemID`, `regionID`, `romHashCRC`, `romHashMD5`,
  `romHashSHA1`, `romSize`, `romFileName`, `romExtensionlessFileName`,
  `romParent`, `romSerial`, `romHeader`, `romLanguage`, `romDumpSource`,
  `lastModified`.
- `RELEASES`: `releaseID` (PK), `romID`, `releaseTitleName`,
  `regionLocalizedID`, cover fields, `releaseDescription`, developer,
  publisher, genre, date, reference URL fields, and `lastModified`.
- `CHEAT_DEVICES`: `cheatDeviceID` (PK), `systemID`, name, brand, format,
  and `lastModified`.
- `CHEAT_CATEGORIES`: `cheatCategoryID` (PK), name, description, and
  `lastModified`.
- `CHEATS`: `cheatID` (PK), `romID`, name, activation, description, side
  effect, folder, `cheatCategoryID`, code, `cheatDeviceID`, credit, and
  `lastModified`.

Declared foreign keys cover ROM system/region, release ROM, device system,
and cheat ROM/category/device. `RELEASES.regionLocalizedID` is not declared as
a foreign key, but every value in the snapshot resolves to `REGIONS`. All
declared and logical relationships have zero orphaned rows. The seven triggers
only maintain each table's `lastModified` field. There are no source indexes
and no application/user version marker (`PRAGMA user_version=0`).

Of 51,748 ROM rows, 10,815 lack each hash type, 31,392 have a null serial and
86 have an empty serial. Every non-null hash has valid CRC32/MD5/SHA-1 syntax.
There are five duplicate groups for each hash algorithm, so a hash lookup can
still be ambiguous. Global code-body grouping finds 22,760 repeated bodies
(60,662 rows beyond the first); ArchiveFS preserves them because different
ROMs, names, categories, or releases may give the same bytes different
meaning.

Observed maxima are 166 bytes for a release title, 2,729 for a release
description, 81 for a cheat name, 384 for a cheat description, 339 for an
activation, 222 for a side effect, and 6,173 for a code body. The adapter's
limits are deliberately above these real maxima while remaining bounded.

The repository is not archived, but its last commit was in April 2023. It does
not declare a dataset licence and its maintenance scripts contain an “All
Rights Reserved” notice. ArchiveFS therefore describes the dataset licence as
not established and does not redistribute it.

## Source safety

Download and local import are explicit user actions. The downloader accepts
only the pinned HTTPS URL on `raw.githubusercontent.com`, refuses redirects,
limits the response to 128 MiB, validates the exact size and SHA-256, checks
the SQLite schema and relationships, and publishes only after validation.

Local import opens a regular file without following its final symlink, copies
it into ArchiveFS-owned storage, and applies the same validation. The selected
original is never modified. A failed replacement leaves the previous database
in place.

Catalogue queries use SQLite `mode=ro`, `immutable=1`,
`SQLITE_OPEN_READ_ONLY`, `query_only=ON`, and `trusted_schema=OFF`. Queries are
parameterised and paginated. ArchiveFS performs no migration, `ATTACH`,
extension load, or source-side index creation. Measurements on the 64 MiB
source showed that bounded scans were sufficient, so Stage 1 does not build a
derived index.

## Matching

The strongest available evidence is used without displacing stronger local or
RomM identity:

1. exact syntactically valid CRC32, MD5, or SHA-1 plus canonical platform;
2. exact serial plus canonical platform and optional region;
3. explicit upstream release ID;
4. exact normalised title plus canonical platform and region;
5. title plus canonical platform;
6. ambiguous or no match.

CheatBase has no explicit revision field. Multiple releases or duplicate hash
records remain ambiguous and are never resolved by selecting the first row.

## Stage 1 limits

- browse only; no Install action, decoding, or format conversion;
- no automatic library hashing;
- no revision certainty;
- only Nintendo DS has cheat rows in the supported snapshot;
- all device formats are potentially convertible, reference-only, or unknown;
  none is directly installable;
- the database is optional and its content licence is not established.

## RomM integration-readiness note

This stage deliberately does not import or anticipate code from the separate
RomM branch. After RomM is merged, reconciliation should be limited to these
well-defined boundaries:

- identity evidence ordering: a verified local or RomM hash/serial remains the
  authority; CheatBase is a candidate source and cannot displace stronger
  evidence;
- provider provenance: retain both the identity source and CheatBase row/source
  fingerprint rather than flattening them into one label;
- hash evidence: reuse the shared normalized algorithm/value representation,
  while keeping malformed and duplicate CheatBase hashes non-authoritative;
- conflicting records: surface conflicts and ambiguity instead of selecting
  the first provider record;
- status models and source cards: reconcile shared lifecycle vocabulary while
  retaining CheatBase's explicit licence and Nintendo-DS-only coverage facts;
- JSON structures: preserve the Stage 1 status fields and add shared identity
  evidence without silently renaming or weakening them;
- canonical platforms: continue resolving explicit CheatBase mappings through
  the single ArchiveFS registry.

No hypothetical conflict is resolved on this branch; this note identifies the
review points for a later rebase only.
