# ArchiveFS Domain Model

This document describes the core ArchiveFS domain types used by `archivefs-core`.

## Library Hierarchy

ArchiveFS models a library as a hierarchy from broad organization down to concrete files and mounts.

```text
Library
 └── Platform
     └── Title
         └── Release
             ├── Archive
             └── Mount
```

- `Library`: the full managed collection.
- `Platform`: a system or platform grouping, such as a console, computer, media type, or preservation set.
- `Title`: the normalized work or game title.
- `Release`: a specific edition, region, version, dump, or package of a title.
- `Archive`: the source archive file for a release.
- `Mount`: the mounted folder view of an archive.

v0.1 implements `Archive` and mount planning directly. `Library`, `Platform`, `Title`, and `Release` are domain concepts reserved for richer indexing and duplicate handling.

## Archive

An `Archive` is one supported archive file discovered by the scanner.

```rust
Archive {
    path,
    kind,
    identity,
    health,
}
```

- `path`: absolute or configured-source-relative path to the archive file.
- `kind`: archive format detected from the filename.
- `identity`: metadata used to distinguish archives beyond filename alone.
- `health`: current archive-level health state.

## ArchiveKind

`ArchiveKind` is the supported archive format.

```rust
ArchiveKind {
    Zip,
    SevenZip,
    Rar,
}
```

v0.1 detects these from `.zip`, `.7z`, and `.rar` file extensions. Split RAR continuation parts are skipped except the main `.rar` or `.part1.rar` archive.

## ArchiveIdentity

`ArchiveIdentity` is the stable identity metadata for an archive.

```rust
ArchiveIdentity {
    display_name,
    normalized_name,
    source_root,
    size_bytes,
    modified_time,
    platform,
    region,
    content_hash,
    archive_hash,
    internal_listing_hash,
}
```

- `display_name`: human-readable archive title derived from the filename without the archive extension.
- `normalized_name`: normalized title used for comparison and mount naming.
- `source_root`: configured source folder where the archive was discovered.
- `size_bytes`: archive file size when available.
- `modified_time`: archive file modification time when available.
- `platform`: optional platform/system hint, reserved for richer identity.
- `region`: optional region/version hint, reserved for richer identity.
- `content_hash`: optional hash of extracted or interpreted content.
- `archive_hash`: optional hash of the archive file itself.
- `internal_listing_hash`: optional fingerprint of the archive's internal file listing.

Identity must not rely on filename alone. Later versions can fill the optional fields as archive inspection improves.

## ArchiveHealth

`ArchiveHealth` describes archive-level health.

```rust
ArchiveHealth {
    Pending,
    Mounted,
    Failed,
    MissingParts,
    Corrupt,
    Unsupported,
    PermissionDenied,
    RetryAvailable,
}
```

- `Pending`: discovered but not mounted or diagnosed.
- `Mounted`: archive is mounted successfully.
- `Failed`: mount or inspection failed.
- `MissingParts`: split archive parts are missing.
- `Corrupt`: archive appears damaged.
- `Unsupported`: archive format or structure is unsupported.
- `PermissionDenied`: ArchiveFS cannot read or mount the archive due to permissions.
- `RetryAvailable`: a failed archive can be retried.

Retryable states are `Failed`, `MissingParts`, and `RetryAvailable`.

## MountPlan

`MountPlan` is the planned relationship between an archive and its mount directory.

```rust
MountPlan {
    archive,
    mount_path,
    state,
}
```

- `archive`: archive to mount.
- `mount_path`: directory under the configured `mount_root`.
- `state`: mount-level state.

Mount paths are generated from safe archive names. Duplicate archive filenames get deterministic suffixes so they do not collide.

## MountState

`MountState` describes the mount path state for status reporting.

```rust
MountState {
    Pending,
    Mounted,
    MountPathExists,
}
```

- `Pending`: mount path is not currently mounted.
- `Mounted`: mount path is currently mounted.
- `MountPathExists`: mount path exists but is not detected as mounted.

## MountBackend

`MountBackend` abstracts mounting and unmounting.

```rust
trait MountBackend {
    fn mount(&self, plan: &MountPlan) -> Result<()>;
    fn unmount(&self, mount_path: &Path) -> Result<()>;
}
```

Mount logic depends on this trait instead of calling a concrete mount tool directly.

## RatarmountBackend

`RatarmountBackend` is the v0.1 mount backend. It invokes `ratarmount` to mount archives and uses the platform unmount tools for unmounting.

Native FUSE, daemon behavior, GUI behavior, and Docker packaging are outside the v0.1 domain model.
