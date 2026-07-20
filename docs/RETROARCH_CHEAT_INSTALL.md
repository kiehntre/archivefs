# RetroArch Cheat Installer

An install journal can be safely reverted with
[`retroarch-cheat-rollback`](RETROARCH_CHEAT_ROLLBACK.md); rollback requires
the explicit journal path and destination root.
Installation journals can be listed and assessed without changing anything
with [`retroarch-cheat-history` and `retroarch-cheat-inspect`](RETROARCH_CHEAT_HISTORY.md).

`archivefs retroarch-cheat-install <local-path>` is the first
write-capable command in the RetroArch cheat pipeline: it can actually
create a cheat file, replace one with a verified backup, and write a
journal of what happened. Every earlier command
([`retroarch-cheat-catalogue`](RETROARCH_CHEAT_CATALOGUE.md)) remains
strictly read-only and unchanged - nothing about this command's
implementation modified that one.

```console
archivefs retroarch-cheat-install /path/to/cheat-catalogue --cheat-destination-root ~/.config/retroarch/cheats --dry-run
archivefs retroarch-cheat-install /path/to/cheat-catalogue --cheat-destination-root ~/.config/retroarch/cheats --yes
archivefs retroarch-cheat-install /path/to/cheat-catalogue --cheat-destination-root ~/.config/retroarch/cheats --yes --replace-different --json
```

## Options

| Flag | Meaning |
| --- | --- |
| `<local-path>` | The local cheat catalogue to install from - same formats as `retroarch-cheat-catalogue` (a `.cht` directory tree or a bounded JSON manifest). Required. |
| `--cheat-destination-root <path>` | The RetroArch cheat root to install into. **Required** for this first milestone - there is no default. |
| `--dry-run` | Compute and report the complete installation result; write nothing. |
| `--yes` | Required before *any* filesystem write. Without it, the command behaves exactly like `--dry-run` and prints a refusal notice - it never partially applies anything. |
| `--replace-different` | Permit replacing an existing destination file whose content differs from the source. Without it, such entries are reported as `skipped_replace_not_allowed` and never touched. |
| `--json` | Print the structured `CheatInstallRun` JSON model (the same shape documented in [`RETROARCH_CHEAT_INSTALL_RESULT.md`](RETROARCH_CHEAT_INSTALL_RESULT.md), now actually populated by a real run instead of only a plan). |

`--dry-run` and omitting `--yes` are equivalent in effect (nothing is ever
written in either case) but are reported identically as `dry_run: true` -
see "Confirmation gate" below.

## Eligibility - never installs a bad match

Only entries the staging preview already marked `install_new`,
`already_installed`, or `replace_different` are ever actionable. This
command never installs:

- **weak matches** - only `exact`/`strong` confidence can become
  `install_new`/`already_installed`/`replace_different`;
- **ambiguous matches** - always `skipped_not_eligible`;
- **unsupported matches** - always `skipped_not_eligible`;
- **unresolved platforms** - a platform hint that does not resolve through
  ArchiveFS's own canonical alias table is `skipped_not_eligible` at
  preview time, and would independently be rejected again
  (`failed_unsafe_path`) if it somehow reached this command's own
  destination reconstruction step;
- **incomplete parses** - a catalogue record with parsing diagnostics is
  never eligible;
- **unsafe paths** - a traversal-style, absolute, or separator-containing
  game name or platform component is rejected at both the preview and the
  install layer, independently; or
- **conflicts** - a destination named by more than one catalogue entry
  (whether flagged by the preview or discovered again by this command's
  own batch-level duplicate tracking) is `skipped_conflict` for *every*
  entry naming it, never resolved by picking one.

## Revalidation - nothing from the preview is trusted as still current

Immediately before any write, this command:

1. Re-reads the source file (via the same bounded, no-follow filesystem
   trait used everywhere else in this codebase) and re-hashes it. Any
   mismatch, or the source no longer being readable at all, is
   `skipped_source_changed`.
2. Reconstructs the destination from scratch - the catalogue record's own
   platform hint (resolved through the same canonical alias table matching
   uses) and game name - and revalidates it through
   [`destination_safety`](RETROARCH_CHEAT_CATALOGUE.md), the shared
   read-only, symlink-aware path validator every write-capable check in
   this codebase uses. Any symlink (root, parent, or final component),
   traversal attempt, wrong file type, or other unsafe destination is
   `failed_unsafe_path`.
3. Re-reads and re-hashes any existing destination content and compares it
   to what the preview captured. Any drift is `skipped_destination_changed`.

The preview's own proposed path string and captured hashes are informative
context only - every decision this command makes is based on what it
observes fresh, right before acting.

## Confirmation gate

Any real write requires `--yes`. Without it (and without `--dry-run`
either), the command:

- writes nothing,
- logs a clear refusal notice,
- still computes and prints the full planned result (including `--json`
  output, when requested), and
- never partially processes entries - the full, deterministic pass over
  every entry always runs; only the "would this actually write" gate
  changes.

## Installing a new file (`install_new`)

Only missing directories beneath the destination root are created - one
level at a time, each freshly re-checked with no-follow metadata
immediately after creation, so a symlink placed there by a concurrent
process is never silently traversed. The new content is written to a
uniquely named temporary file *in the same directory* as the final
destination (never a shared, predictable path), flushed and `fsync`ed, then
hash-verified against the source - only after that verification succeeds
does the file become visible at its real name, via an atomic rename that
refuses to clobber an unexpected concurrent creation (Linux
`renameat2(..., RENAME_NOREPLACE)`, with a narrower fallback elsewhere).
The containing directory is then `fsync`ed on a best-effort basis. Only
after every step succeeds is `installed_new` reported; any failure along
the way removes the temporary file and reports the specific failure
outcome instead.

## Already installed (`already_installed`)

No write of any kind happens - not even a timestamp or permission change.
Existing content is only read, hashed, and compared.

## Replacing different content (`replace_different`)

Without `--replace-different`, nothing is touched and the entry is
reported `skipped_replace_not_allowed`.

With `--replace-different`:

1. The existing destination is re-verified against the hash the preview
   captured, then copied into a **backup** - written the same
   temp-file/`fsync`/verify/atomic-rename way as a new install, under a
   dedicated ArchiveFS-managed backup directory (never beside the source
   or destination files themselves): `<original filename>.<run
   ID>.<previous-content-hash prefix>.bak`, guaranteeing no collision
   between runs or between two different previous contents of the same
   file.
2. Only once that backup is durably verified in place is the replacement
   itself written and verified, the same way a new install is, and then
   atomically substituted for the original.
3. The original destination is never opened for writing at any point in
   this sequence - it is only ever read (to create the backup) or
   ultimately replaced by the one atomic, already-verified rename. There is
   therefore no code path in this command that can leave the original
   partially modified: at every point before that final rename, the
   original is either fully intact or already safely backed up.

If any step after the backup exists fails, the backup is always preserved,
the original is left exactly as it was, and the specific failure outcome
(`failed_backup`, `failed_write`, or `failed_verification`) is reported -
never silently treated as success.

## Journal

A real (non-dry-run, confirmed) run writes exactly one journal file -
the same [`CheatInstallRun`](RETROARCH_CHEAT_INSTALL_RESULT.md) JSON model
`--json` prints - under a dedicated `cheat-install-runs` directory inside
the ArchiveFS XDG data directory (`~/.local/share/archivefs/`, alongside
the library database; backups live in a sibling `cheat-install-backups`
directory). The journal is written through a temporary file, `fsync`ed,
and atomically finalized the same never-clobber way as an installed cheat
file - it never overwrites an existing journal (a second run with the same
run ID and journal directory fails clearly instead of silently replacing
the first journal). A dry run (or an unconfirmed run without `--yes`)
writes no journal at all.

If the journal write itself fails after files were genuinely installed,
that failure is reported clearly and separately (the CLI exits non-zero)
without retroactively changing what the run's own entries and summary
correctly say happened to the filesystem - a journal failure never "hides"
a real install, and a real install is never claimed once a journal
failure is reported as if nothing had gone wrong.

## Outcomes

The same stable, lower-snake-case `outcome` values documented in
[`RETROARCH_CHEAT_INSTALL_RESULT.md`](RETROARCH_CHEAT_INSTALL_RESULT.md#outcome-codes)
apply here, now genuinely reachable for a real run:

- `installed_new`, `already_installed`, `replaced_with_backup` - the three
  actionable outcomes above.
- `skipped_replace_not_allowed`, `skipped_not_eligible`, `skipped_conflict`
  - decided without any filesystem write.
- `skipped_source_changed`, `skipped_destination_changed` - revalidation
  rejected a stale preview result.
- `failed_unsafe_path` - `destination_safety` rejected the reconstructed
  destination.
- `failed_backup`, `failed_write`, `failed_verification` - a real write
  attempt failed at a specific stage; see the outcome's own `reason_code`
  and `detail` for exactly which one.

## Example JSON outcomes

An eligible new install, applied for real:

```json
{
  "source_path": { "display": "/catalogue/Frogger.cht", "lossy": false },
  "destination_path": { "display": "/cheats/Atari2600/Frogger.cht", "lossy": false },
  "previous_destination_state": "absent",
  "outcome": "installed_new",
  "reason_code": "destination_missing",
  "applied": true,
  "eligible": true,
  "write_required": true,
  "resulting_destination_hash": "..."
}
```

A weak match - never a staging or install candidate, regardless of flags:

```json
{
  "outcome": "skipped_not_eligible",
  "reason_code": "weak_match_not_eligible",
  "applied": false,
  "eligible": false,
  "write_required": false,
  "destination_path": null
}
```

A replacement blocked by policy:

```json
{
  "outcome": "skipped_replace_not_allowed",
  "reason_code": "replace_different_not_permitted",
  "applied": false,
  "eligible": true,
  "write_required": true
}
```

**Weak and ambiguous matches are never installed, with or without
`--yes` or `--replace-different` - eligibility is decided entirely by
match confidence and safety, never by which flags were passed.**

## Non-goals

This command does not:

- install a `weak`, `ambiguous`, or `unsupported` match, under any flag
  combination;
- follow a symlink anywhere in the destination root, parent chain, or
  final component - every such case is `failed_unsafe_path`;
- create anything beyond the directories strictly required to hold the
  destination file;
- trust the staging preview's captured hashes or destination state without
  re-verifying them immediately before writing;
- overwrite an existing destination without `--replace-different` and a
  verified backup already in place;
- overwrite an existing journal file; or
- claim success when the journal write itself failed.

Reusable safety logic - destination-path resolution, symlink rejection,
and the installation result/journal data model - lives entirely in
[`destination_safety`](RETROARCH_CHEAT_CATALOGUE.md) and
[`RETROARCH_CHEAT_INSTALL_RESULT.md`](RETROARCH_CHEAT_INSTALL_RESULT.md)
respectively; this command reuses both without redefining either.
