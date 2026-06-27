# ArchiveFS Architecture

ArchiveFS is split into separate crates.

## Crates

- archivefs-core: config, scanning, mount logic, archive identity, health state
- archivefs-cli: command-line interface
- archivefs-daemon: folder watcher and automatic mounting
- archivefs-gui: desktop interface, later

## Backend strategy

v0.1 uses ratarmount as the mount backend.

Later versions may replace it with native Rust FUSE support.

## Archive health states

- Pending
- Indexing
- Mounted
- Failed
- MissingParts
- Corrupt
- Unsupported
- PermissionDenied
- SourceChanged
- RetryAvailable

Failed archives must be retryable manually and automatically retried when the source file changes.

## Duplicate strategy

Never rely on filename alone.

Archive identity should use:

- source path
- platform/system where known
- normalized title
- region/version where known
- archive size
- archive hash when available
- internal file listing fingerprint
