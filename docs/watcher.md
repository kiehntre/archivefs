# ArchiveFS Watcher

This document designs the ArchiveFS filesystem watcher.

The watcher observes configured source folders, refreshes `ArchiveRecord` data,
and updates the JSON index. It must not mount or unmount archives.

## Goals

- Keep ArchiveFS aware of archive files as they are added, removed, renamed, or
  modified.
- Update `ArchiveRecord` values and the JSON index without requiring manual
  `index-build` runs.
- Preserve current mount state information when refreshing records.
- Handle large download batches without repeatedly rescanning the full library.
- Ignore temporary, incomplete, and transient download files.
- Keep watcher behavior independent of CLI command execution.
- Provide a Linux-first implementation while leaving room for cross-platform
  backends.

## Linux-First Implementation

The first watcher should target Linux using the Rust `notify` crate backed by
`inotify`.

Linux behavior:

- Watch each configured source folder recursively.
- Add watches for newly created directories.
- Remove watches for deleted directories.
- Treat `IN_MOVED_TO`, `IN_CLOSE_WRITE`, and relevant create events as possible
  archive additions or updates.
- Treat `IN_DELETE` and `IN_MOVED_FROM` as possible archive removals.
- Treat queue overflow as a signal to perform a safe broader rescan.
- Normalize raw `notify` events into ArchiveFS-level changes before processing.

Normalized events should include:

- Created path.
- Modified path.
- Removed path.
- Renamed path.
- Directory created.
- Directory removed.
- Watch overflow or missed events.

Raw events should not directly mutate the index. They should enqueue paths for
debounced processing.

The Linux implementation should remain isolated behind a watcher abstraction so
later platform backends do not affect core record/index logic.

## Debouncing Repeated Filesystem Events

Filesystems often emit many events for one user action. Downloads and extracts
can generate hundreds or thousands of events.

Watcher processing should debounce by path and by source root:

- Collect events into a short debounce window.
- Coalesce repeated events for the same path.
- Prefer the final known operation when possible.
- Expand directory events into a targeted scan of that directory after the
  debounce window.
- Use a longer quiet period for paths that are still changing.

Debouncing should prevent repeated index writes during bulk imports. A batch of
many files should normally produce one index update or a small bounded number of
updates.

## Partial Download Handling

The watcher must avoid indexing incomplete downloads.

It should treat archives as unstable when:

- File size is still changing.
- Modified time is still changing.
- The file is open for writing when the platform can detect that.
- A matching temporary sidecar file exists.

Stable archive detection should require a quiet period. For large downloads, the
quiet period should be long enough to avoid indexing files between chunks.

Partial download handling must be conservative. It is better to delay indexing a
new archive than to add an incomplete archive to the index.

## Temporary File Patterns To Ignore

The watcher should ignore known temporary download and sync patterns such as:

- `.part`
- `.partial`
- `.crdownload`
- `.download`
- `.tmp`
- `.temp`
- `.!qB`
- `.aria2`
- Files ending in `~`
- Hidden temporary files created by sync tools.

Temporary files should be filtered before archive detection. A temporary file
that happens to end with a supported archive extension should still be ignored
until it is renamed to a stable final filename.

## Safe Rescan Strategy

The watcher should prefer targeted rescans.

Targeted rescan rules:

- For a changed archive path, rescan only that path and its parent directory
  when needed.
- For a changed directory, rescan that directory recursively.
- For a delete event, remove matching records by path after confirming the file
  no longer exists.
- For rename events, treat the old path as removed and the new path as added.

Fallback to a full source rescan when:

- The watcher reports overflow or missed events.
- A watched source root is replaced.
- The internal watcher state becomes inconsistent.
- Too many directories change in one batch and targeted scanning would be more
  expensive than a full rescan.

Safe rescans should rebuild records using the same provider pipeline as other
ArchiveFS entry points. The watcher should not have separate metadata or health
logic.

## Index Update Strategy

The watcher should update only `ArchiveRecord` data and the JSON index.

Index update rules:

- Never mount automatically.
- Never unmount automatically.
- Do not call CLI commands.
- Use the same JSON index format until an explicit index format migration is
  designed.
- Write index updates atomically through a temporary file and rename.
- Avoid writing the index when the computed content has not changed.
- Coalesce many record changes into one index write.

The index writer should tolerate the index being missing. In that case, the
watcher may create it from current records.

If another ArchiveFS process updates the index concurrently, the watcher should
avoid corrupting the file. A future lock file or advisory lock should coordinate
writers.

## Mount State Preservation

Watcher refreshes must preserve mount state accurately.

The watcher should:

- Recompute `MountPlan` values for current archives.
- Read current mounted paths from the filesystem.
- Set `MountState` based on observed mount state.
- Keep mounted archives mounted.
- Keep unmounted archives unmounted.
- Not remove mount directories.

If an archive disappears while mounted, the watcher should update records and
the index but must not unmount the mount path. Cleanup remains an explicit user
action.

## Error Handling

Watcher errors should be visible but isolated.

- Watch setup failure for a source root should be reported and retried.
- Temporary permission errors should not stop the entire watcher.
- Provider failures should follow provider pipeline error rules.
- Index write failures should be logged and retried with backoff.
- Watch overflow should trigger a broader rescan.
- Repeated errors should be rate-limited.

The watcher should continue processing healthy source roots when one source root
fails.

Errors must not trigger automatic mounting, unmounting, or destructive cleanup.

## Performance Considerations

The watcher should be efficient for large libraries and large download batches.

- Use debounced batches instead of per-event scans.
- Use targeted rescans whenever reliable.
- Avoid hashing or deep archive inspection unless configured providers require
  it.
- Cache provider results using archive path, size, modified time, provider name,
  and provider version.
- Limit concurrent scans to avoid saturating disks.
- Bound memory used by pending event queues.
- Collapse very large event batches into a full source rescan.
- Avoid repeated JSON writes for unchanged index content.

The default watcher should be cheap enough to run continuously.

## Testing Strategy

Testing should separate event normalization from OS-specific watchers.

Tests should cover:

- Debouncing repeated events for one archive.
- Ignoring temporary download files.
- Waiting for file size and modified time stability.
- Adding archives after stable downloads complete.
- Removing records for deleted archives.
- Rename handling.
- Directory creation and recursive watch registration.
- Queue overflow fallback to full rescan.
- Large batch coalescing.
- Index write coalescing.
- Preservation of mounted and unmounted states.
- Provider error handling.

Linux integration tests can exercise `inotify` where available. Unit tests
should use fake event sources so they do not require platform-specific watcher
APIs.

Tests must assert that watcher operations never call mount or unmount backends.

## Future Cross-Platform Support

Cross-platform support should add backends behind the same watcher abstraction.

macOS:

- Use `FSEvents`.
- Expect directory-level event granularity.
- Use targeted rescans after normalized directory events.

Windows:

- Use `ReadDirectoryChangesW`.
- Normalize rename pairs where possible.
- Handle case-insensitive paths carefully.

All platforms should share:

- Event debounce logic.
- Partial download filtering.
- Safe rescan strategy.
- Provider pipeline execution.
- JSON index update logic.

Platform differences should remain in event collection and path normalization,
not record construction.

## Design Principles

- The watcher observes and indexes; it does not mount.
- The watcher observes and indexes; it does not unmount.
- ArchiveRecords are the watcher output model.
- The JSON index is the watcher persistence target.
- CLI commands and watcher loops are independent entry points over shared core
  logic.
- File events are hints, not truth.
- Stable rescans are more important than immediate updates.
- Incomplete downloads should stay invisible.
- Batch work should be coalesced.
- Safety wins over convenience.
