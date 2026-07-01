# ArchiveFS Provider Pipeline

This document describes how ArchiveFS providers should cooperate when building
`ArchiveRecord` values.

Providers are extension points for deriving archive metadata, archive health,
and later other record-level information. The pipeline must keep core behavior
predictable while allowing richer local and external sources to participate.

## Goals

- Keep `ArchiveRecord` as the shared record model for CLI, index, watcher,
  plugins, SQLite, and GUI workflows.
- Allow multiple providers to contribute metadata and health without each
  command rebuilding archive knowledge independently.
- Preserve deterministic output for the same archive, config, providers, and
  cache state.
- Prefer local, cheap, and deterministic providers before expensive or external
  providers.
- Make provider failures observable without making one optional provider break
  unrelated commands.
- Keep CLI and JSON output stable until an explicit output format change is
  designed.

## Execution Order

Providers should run in a fixed pipeline order:

1. Scan archives from configured source folders.
2. Build `MountPlan` values from scanned archives.
3. Determine observed `MountState` from the filesystem.
4. Run metadata providers.
5. Run health providers.
6. Merge provider results into `ArchiveRecord`.
7. Project records into command-specific outputs such as status rows or JSON
   index entries.

Within each provider category, providers should run in registered priority
order. The default core providers should run first:

- `FilenameMetadataProvider`
- `FilesystemHealthProvider`

Providers that need information from earlier providers should consume the
partially built record context rather than re-scan the filesystem.

## Priority Rules

Provider priority should be explicit and stable.

- Core providers have the baseline priority.
- User-configured providers run after core providers unless explicitly
  configured otherwise.
- Plugins must declare a default priority.
- Two providers with the same priority run in registration order.
- Priority affects merge order, not whether a provider is trusted.

Priority should not be used as a hidden correctness mechanism. If one provider
must always override another, that relationship should be documented in the
provider registration or config.

## Conflict Resolution

Conflicts happen when two providers return different values for the same field.

ArchiveFS should resolve conflicts using field-level rules:

- Empty values never override populated values.
- Higher-priority providers may override lower-priority providers only for
  fields they explicitly claim.
- Equal-priority conflicts keep the first registered value.
- Conflicts should be recorded in diagnostics when useful, especially for
  metadata fields shown in user interfaces.
- Core safety fields should prefer conservative values over optimistic values.

Provider output should include enough provenance in the future to explain why a
field has its final value.

## Metadata Merging

Metadata merging should be field-based, not all-or-nothing.

Initial metadata fields:

- `title`
- `platform`
- `region`
- `languages`
- `version`
- `disc`
- `publisher`
- `developer`
- `release_year`
- `genre`
- `notes`
- `source`

Merge rules:

- `title`: prefer explicit user or database metadata over filename metadata.
- `platform`: prefer configured or directory-derived metadata over weak
  filename heuristics.
- `region`, `version`, and `disc`: preserve specific values over generic
  values.
- `languages`: merge as a stable deduplicated list.
- `publisher`, `developer`, `release_year`, and `genre`: prefer authoritative
  catalog or database providers over filename providers.
- `notes`: append only when the notes are provider-scoped or clearly
  non-duplicative.
- `source`: identify where the metadata came from; do not treat it as a user
  visible title/source path replacement.

Providers should return only fields they can justify. Unknown fields should
remain `None`.

## Health Merging

Health merging should be conservative.

Archive health states, from least concerning to most concerning for safety:

- `Mounted`
- `Pending`
- `RetryAvailable`
- `Failed`
- `MissingParts`
- `Unsupported`
- `PermissionDenied`
- `Corrupt`

Suggested rules:

- Terminal or safety-related states override optimistic states.
- `Corrupt`, `PermissionDenied`, and `Unsupported` should not be downgraded by
  later providers unless a higher-trust provider explicitly proves recovery.
- `MissingParts` should override `Pending` and `Mounted` for split archives.
- `Mounted` may describe current mount success, but it should not hide archive
  integrity problems reported by another provider.
- `RetryAvailable` should be used when the provider knows a retry is meaningful.

Health providers should distinguish between "no opinion" and `Pending`. A
future health result type should support this directly so providers are not
forced to report `Pending` when they did not inspect the archive.

## Error Handling

Provider errors should be isolated and classified.

- Required core provider failures may fail the operation.
- Optional provider failures should attach warnings or diagnostics and allow the
  pipeline to continue.
- Provider timeouts should be treated as provider failures, not archive
  failures.
- A provider must not panic for normal archive data.
- Errors should include provider name, archive path, and operation context.
- Repeated provider errors should be rate-limited in CLI output and logs.

An archive-level health problem is not the same as a provider failure. For
example, "archive is corrupt" is valid health output; "provider crashed while
checking archive" is a provider error.

## Plugin Registration

Plugins should register providers through a central registry.

Registration should include:

- Provider name.
- Provider kind, such as metadata or health.
- Default priority.
- Whether the provider is required or optional.
- Capabilities and required permissions.
- Cache policy.
- Timeout policy.
- Version and compatibility range.

Registration must be deterministic. Loading plugins in a different filesystem
order should not change provider order unless the user config says so.

Plugins should not mutate global ArchiveFS state during registration. They
should describe capabilities first, then execute only when the pipeline invokes
them.

## Future External Providers

External providers may query databases, web services, APIs, or user-managed
catalogs.

External providers must be opt-in. They should declare:

- Network usage.
- Data sent outside the machine.
- Authentication requirements.
- Rate limits.
- Cache behavior.
- Failure behavior.

External results should be cached and merged with lower trust than explicit
local user metadata unless configured otherwise.

ArchiveFS should support offline operation. Lack of network access must not
break scanning, status, mounting, unmounting, or local index commands.

## Caching

Provider caching should avoid repeated expensive work while keeping results
fresh.

Cache keys should include:

- Archive path.
- Source root.
- Archive size.
- Modified time.
- Provider name and version.
- Provider configuration hash.

Providers that inspect archive contents may also include archive hash or
internal listing hash when available.

Cache entries should store:

- Provider result.
- Provider diagnostics.
- Timestamp.
- Input fingerprint.
- Expiration policy.

Cache invalidation should be conservative. If the archive file changed, cached
metadata and health from content-sensitive providers should be considered stale.

## Performance

The default pipeline should remain fast for large libraries.

- Filename and filesystem providers should be cheap and synchronous.
- Expensive providers should be bounded by timeouts and concurrency limits.
- Providers should avoid reopening or rehashing archives when earlier pipeline
  stages already collected enough information.
- Batch-capable providers should receive batches where practical.
- External providers should use caching by default.
- CLI commands should avoid running expensive optional providers unless the
  command needs their output.

The pipeline should make cost visible. Future provider registration should allow
providers to declare cost classes such as cheap, filesystem inspection, archive
inspection, database lookup, or network.

## Testing Strategy

Provider behavior should be tested at three levels:

- Unit tests for individual providers.
- Merge tests for metadata, health, priority, and conflict behavior.
- End-to-end tests that prove existing CLI and JSON projections remain stable.

Provider injection helpers should allow tests to supply deterministic fake
providers. Tests should cover:

- Empty provider output.
- Conflicting metadata values.
- Conservative health merges.
- Provider failure isolation.
- Priority ordering.
- Cache hits, misses, and invalidation.
- Stable output ordering.

External provider tests should not require network access by default.

## Design Principles

- Records are the shared product of the pipeline.
- Providers contribute facts; merge policy decides final values.
- Local deterministic information is the baseline.
- Optional enrichment must not break core archive operations.
- Unknown values should remain unknown.
- Prefer explicit provenance over hidden assumptions.
- Make provider order and conflict behavior predictable.
- Keep output format changes separate from internal model changes.
- Design for future plugins without requiring plugins in the core path.
