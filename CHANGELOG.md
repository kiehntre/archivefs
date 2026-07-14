# Changelog

## v0.2.0-alpha

### Added

- Desktop GUI: **Mount All**, a sequential bulk-mount workflow that reports per-archive outcomes and stops cleanly on failure
- Desktop GUI: **Unmount All**, the equivalent sequential bulk-unmount workflow, with optional cleanup of the archive's mount directory afterward
- Lazy-unmount recovery for mounts that are busy at unmount time, with a follow-up offer to remount once the previous mount has been released
- Activity panel in the GUI recording recent mount, unmount, and setup operations, with a Clear action
- First-run Setup flow and a startup Diagnostics report that check the config file, mount root, and required tools before archive actions are allowed
- `status --json` output, joining the existing `stats --json`, `info --json`, and `doctor --json`

### Changed

- README refreshed with a new project banner image and updated description

### Fixed / Safety

- The GUI now retains and can display the last known good snapshot when a background refresh fails, marking it stale instead of discarding it
- Mount and unmount actions are gated on a coherent config identity check (config path plus a SHA-256 digest of its contents), so actions are blocked if the on-disk config changed since the snapshot and diagnostics were last read

## v0.1.0-alpha

### Added

- Linux-first ArchiveScanner
- Read-only archive mounting
- JSON archive index
- File watcher
- Provider pipeline
- Duplicate detector framework
- Filename duplicate detector
- `doctor`
- `config-check`
- `stats`
- `info`
- `duplicates`
- `status`
- `watch`
- JSON output:
  - `stats --json`
  - `info --json`
  - `doctor --json`

### Quality

- GitHub Actions CI
- Clippy clean
- 59 unit tests
- Architecture documentation
- JSON API documentation
