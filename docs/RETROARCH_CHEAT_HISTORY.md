# RetroArch Cheat Installation History

ArchiveFS can discover and inspect its RetroArch cheat installation journals
without opening their JSON files manually:

Journals produced by guided
[`retroarch-cheat-setup`](RETROARCH_CHEAT_SETUP.md) use this same schema and
default history root; there is no separate setup history implementation.
Installs originating from a validated
[trusted-source snapshot](RETROARCH_CHEAT_SOURCES.md) record its stable source
ID and are discovered normally without network access.

```console
archivefs retroarch-cheat-history
archivefs retroarch-cheat-history --journal-root /path/to/cheat-install-runs
archivefs retroarch-cheat-history --json
archivefs retroarch-cheat-inspect ~/.local/share/archivefs/cheat-install-runs/<run>.json
archivefs retroarch-cheat-inspect ~/.local/share/archivefs/cheat-install-runs/<run>.json --json
```

Both commands are strictly read-only. They do not install, remove, restore,
repair, migrate, or rewrite anything; do not update timestamps; and do not
create a missing journal directory. To change installed files, use the
separately gated [`retroarch-cheat-install`](RETROARCH_CHEAT_INSTALL.md) and
[`retroarch-cheat-rollback`](RETROARCH_CHEAT_ROLLBACK.md) commands.

## Journal locations

`retroarch-cheat-history` defaults to the same directory the installer uses:

```text
~/.local/share/archivefs/cheat-install-runs/
```

This is resolved beneath the same home-directory data location used by the
current installer. `--journal-root <path>` selects
an explicit installation-journal directory for history discovery. It does not
change the expected ArchiveFS backup or rollback-journal roots.

A missing history directory is a successful, empty result and is not created.
A symlinked, inaccessible, or otherwise unsafe root produces a warning and no
entries. Only direct-child files whose names end in `.json` are considered;
other files and subdirectories are deliberately ignored.

`retroarch-cheat-inspect` is intentionally stricter. Its path must identify a
plain direct-child file under the default installation-journal root. A
malformed, unsupported, inaccessible, symlinked, out-of-root, or path-unsafe
journal exits non-zero. With `--json`, stdout still contains a structured
failure envelope (`ok: false` and an `error` object); normal CLI diagnostics
remain on stderr.

## Output

Human-readable history lists each run newest first, followed by its source,
destination, original action, current destination state, backup state, and
rollback availability. Invalid candidates appear as concise warnings instead
of raw JSON. Equal or absent timestamps use stable journal-path ordering.

`--json` emits the versioned `CheatHistoryReport` model. History contains all
valid run inspections plus structured `warnings`. Inspect emits a versioned
envelope with `ok` and either `inspection` or `error`. Paths use an ArchiveFS
`{ "display": ..., "lossy": ... }` representation. On Unix, a non-UTF-8
history path additionally carries `raw_bytes`, preserving its exact identity
for core/GUI consumers. Security decisions always use the original
operating-system path, never the display string.

Install journals record a catalogue source and source path but no separate
game-title field. Inspection therefore exposes the source filename stem as
`display_title`, and derives `platform` from the validated platform component
of the destination. It also retains the recorded source and destination paths.

## Destination assessments

| Value | Meaning |
| --- | --- |
| `unchanged_since_install` | A safe regular destination file still has the installed hash. |
| `missing` | No destination entry exists. For `installed_new`, rollback is already unnecessary; for a replacement, the unexpected absence blocks rollback. |
| `changed` | A safe regular destination exists but its hash differs. ArchiveFS will not overwrite it during rollback. |
| `inaccessible` | Metadata or content could not be read; missing is reported separately. |
| `unsafe_path` | Root binding, traversal, symlink, component, or file-type safety failed. The file is not hashed. |
| `unknown` | The journal does not provide enough safe information for an assessment. |

Replacement entries also report backup state: `present_and_valid`, `missing`,
`changed`, `inaccessible`, `unsafe_path`, `unknown`, or `not_applicable`.
Backups must be direct children of ArchiveFS's expected
`cheat-install-backups` root and match the recorded previous hash.

## Rollback availability

| Value | Meaning |
| --- | --- |
| `available` | Current destination and any required backup satisfy the rollback preconditions. |
| `unnecessary` | The install made no change, an `installed_new` destination is absent, or a replacement already has its recorded previous hash. |
| `already_completed` | Exactly one strongly bound rollback journal records a successful completed rollback. |
| `blocked_destination_changed` | The destination is changed, or a replacement destination is unexpectedly missing. |
| `blocked_missing_backup` | A replacement's required backup is absent. |
| `blocked_backup_changed` | The backup hash no longer matches. |
| `blocked_unsafe_path` | Destination or backup path safety failed. |
| `blocked_invalid_journal` | Required install metadata is internally incomplete. |
| `unknown` | Inaccessibility or ambiguity prevents a safe conclusion. |

Rollback journals are not associated by filename. ArchiveFS validates the
recorded install run ID, original journal path, destination root, and every
entry's destination, expected installed/previous hashes, and backup path.
Multiple fully bound records are reported as ambiguous; a record that merely
names the same run ID does not establish completion.

## Malformed data and path safety

History skips malformed, unsupported, inaccessible, and unsafe `.json`
candidates while continuing with other journals. Inspect fails on the same
conditions. Neither command repairs the data.

Journal, destination, backup, and rollback-journal paths are attacker-controlled
inputs. Inspection rejects traversal and root escapes, rejects symlinked roots,
parents, journals, destinations, and backups, distinguishes missing from I/O
failure, and never hashes a file reached through a rejected path. Lossy paths
from journal JSON are descriptive only and cannot be reconstructed for a
security decision. The checks narrow but cannot eliminate filesystem
time-of-check/time-of-use races; inspection performs no subsequent write.
