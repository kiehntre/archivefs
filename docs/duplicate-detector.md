# Duplicate Detector Plugin Architecture

This document describes the intended architecture for duplicate detection in ArchiveFS. It is a design document only. Duplicate detection is not implemented yet.

## Goals

The duplicate detector should help users identify archives that may represent the same content, title, release, or file without making destructive decisions for them.

Goals:

- Detect likely duplicate archives using reusable ArchiveFS models.
- Avoid filename-only decisions where better identity data is available.
- Produce clear reports that explain why archives were grouped together.
- Support different confidence levels instead of a single duplicate/not-duplicate answer.
- Keep duplicate detection separate from scanning, mounting, unmounting, and cleanup.
- Leave room for future metadata and hash providers.
- Never delete, move, rename, rewrite, mount, or unmount archives automatically.

## What Constitutes a Duplicate

A duplicate is any group of two or more archives that appear to represent the same file, title, platform release, ROM, or content item according to a specific matching rule.

Duplicate detection should report candidates, not perform cleanup. A duplicate group should always include the matching reason and confidence level so users can decide what to do.

Examples of duplicate signals:

- Two archive files have the same archive hash.
- Two archives have the same normalized title.
- Two archives have the same normalized title and platform.
- Two archives expose the same CRC from internal metadata.
- Two archives contain ROMs with the same content hash.
- Metadata providers identify the same external game or release ID.

Some signals are weak. For example, title-only matches can produce false positives across platforms, regions, editions, and unrelated games with similar names. The detector should treat these as lower-confidence candidates.

## Duplicate Levels

Duplicate levels are ordered from file-level certainty toward looser semantic matches.

### Exact Archive Duplicates

Exact archive duplicates are archives with the same archive-level identity. In the future this should use an archive file hash when available. Before hashing is implemented, exact archive detection may be limited to conservative filesystem identity signals such as identical size and path-derived identity, but it should not claim byte-level equality without a hash.

Expected confidence: high when backed by archive hash.

### Same Title

Same-title duplicates share the same normalized title or provider title.

This level is useful for finding obvious clutter but is not enough to prove duplication. The same title may exist on multiple platforms, in multiple regions, or in different editions.

Expected confidence: low to medium.

### Same Title + Platform

Same-title-plus-platform duplicates share a normalized title and the same detected or provider-supplied platform.

This is stronger than title-only matching and should be the main early duplicate candidate level before hashes are available.

Expected confidence: medium.

### Same CRC (Future)

Same-CRC duplicates share a CRC value extracted from archive contents, ROM headers, DAT files, or provider data.

CRC can be useful for ROM collections and preservation sets, but the detector should record where the CRC came from and what file it describes. CRC alone may not identify full archive equality.

Expected confidence: high for the specific file or ROM the CRC describes.

### Same ROM Hash (Future)

Same-ROM-hash duplicates share a strong content hash of the ROM or primary payload inside the archive.

This should be treated as a high-confidence content duplicate even when archive containers, compression formats, filenames, or metadata differ.

Expected confidence: high.

## Why Duplicate Detection Is a Plugin

Duplicate detection should be a plugin because it is policy-heavy and likely to evolve faster than the scanner and mount engine.

Reasons:

- Different users care about different duplicate definitions.
- Hashing can be expensive and should be optional.
- Metadata-assisted matching may require external providers or network-backed data.
- ROM-focused duplicate rules differ from media, preservation, and general archive rules.
- Duplicate detection should not complicate the core scan/mount path.
- New matching strategies should be addable without hardcoding them into `archivefs-core`.

The core should provide stable input models and report structures. Plugins should provide matching strategies.

## Consuming ArchiveRecord

The duplicate detector should consume `ArchiveRecord` values produced by `ArchiveScanner` or the JSON index rebuild path.

Useful `ArchiveRecord` fields include:

- `identity.display_name`
- `identity.normalized_name`
- `identity.source_root`
- `identity.size_bytes`
- `identity.modified_time`
- `identity.platform`
- `identity.region`
- `identity.archive_hash`
- `identity.content_hash`
- `identity.internal_listing_hash`
- `metadata.title`
- `metadata.platform`
- `metadata.region`
- `metadata.version`
- `metadata.disc`
- `metadata.source`
- `mount_plan.archive.path`
- `health`
- `mount_state`

The plugin should treat `ArchiveRecord` as read-only input. It should not rescan the filesystem unless explicitly given a provider that performs additional inspection. It should not mount or unmount archives.

A basic flow could be:

```text
ArchiveScanner
  -> Vec<ArchiveRecord>
  -> DuplicateDetectorPlugin
  -> DuplicateReport
  -> CLI / JSON / health integration
```

## Report Structure

Duplicate reports should be structured enough for CLI output, JSON output, tests, and future UI use.

A report should include:

- Detector name and version.
- Input summary: archive count and timestamp when generated.
- Duplicate groups.
- Warnings or skipped checks.

Each duplicate group should include:

- Group ID.
- Duplicate level.
- Confidence.
- Match reason.
- Shared key used for grouping.
- Candidate archives.

Each candidate archive should include:

- Archive path.
- Display title.
- Platform.
- Size.
- Modified time.
- Health.
- Mount state.
- Optional metadata source.
- Optional hashes or CRCs used by the detector.

Example shape:

```text
DuplicateReport
  detector: "title-platform"
  archives_checked: 128
  groups:
    - level: SameTitlePlatform
      confidence: Medium
      key: "Xbox360::007_Legends"
      reason: "same normalized title and platform"
      candidates:
        - archive_path: /data/xbox360/007 Legends.zip
        - archive_path: /data/imports/007_Legends.7z
```

Reports should explain uncertainty. For weak matches, wording should use candidate language such as "possible duplicate" rather than "duplicate".

## Health and Status Integration

Duplicate detection should not automatically change archive health in the first implementation. Duplicate status is advisory, not a mount failure.

Possible future integrations:

- Add duplicate summary counts to stats or a dedicated duplicate command.
- Add a duplicate warning field to an archive report.
- Show whether an archive belongs to one or more duplicate groups.
- Expose duplicate candidates in `archivefs info`.
- Add a non-fatal health/status category such as `DuplicateCandidate` only after the semantics are clear.

Any health/status integration should avoid implying that an archive is broken. Duplicate candidates may be valid alternate regions, revisions, or formats.

## Future Metadata-Assisted Duplicate Detection

Future metadata providers can improve duplicate detection by supplying stable external identifiers and richer release data.

Metadata-assisted signals may include:

- Provider title IDs.
- Platform IDs.
- Release IDs.
- Region and language data.
- Edition, revision, disc, or version fields.
- Publisher/developer/release-year context.
- Known DAT or preservation-set identifiers.

Potential provider-assisted matching:

- Same provider title ID.
- Same provider title ID plus platform ID.
- Same release ID.
- Same region/language/revision group.
- Same DAT entry.

Metadata-assisted matching should record which provider supplied the signal. If providers disagree, the report should preserve that disagreement rather than hiding it.

## Non-Goals

The duplicate detector should not:

- Delete archives.
- Move archives.
- Rename archives.
- Rewrite archives.
- Mount archives to inspect them unless a future explicit inspection provider is configured.
- Unmount archives.
- Treat weak title matches as proof.
- Block normal mounting by default.
