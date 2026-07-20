# ArchiveFS Database Diagnosis and Recovery

ArchiveFS stores its local catalogue at
`~/.local/share/archivefs/library.sqlite3`. The path is resolved from `HOME`
(or `USERPROFILE` when `HOME` is unavailable) plus
`.local/share/archivefs/library.sqlite3`; it is not currently configurable and
does not consult `XDG_DATA_HOME`. Resolving the path creates nothing.

The catalogue is valuable user data even though mount/unmount safety never
depends on it. Diagnosis must preserve evidence. Recovery is therefore never
automatic.

## Safe diagnosis

Run:

```sh
archivefs database-check
archivefs database-check --json
```

The command opens only an existing regular file with SQLite's explicit
read-only flag. It reports the main file, `-journal`, `-wal`, and `-shm`
sidecars and their sizes, journal mode, schema version, and a bounded
`quick_check`. It does not create the database or parent directory, run
migrations, write pragmas, change journal mode, checkpoint WAL, begin a write
transaction, remove sidecars, or repair anything. The JSON's raw SQLite prose
is marked unstable; use its diagnostic codes for automation.

For a rollback journal, the report also reads only the bounded eight-byte
header with a final-component no-follow open. `hot_candidate` means SQLite's
rollback-journal magic is present; SQLite remains authoritative because locks
and the rest of the header also affect recovery. `zeroed_non_hot` and
`truncated_non_hot` are explicit non-hot states. `malformed` means an
unrecognised non-zero header, while `unreadable` means ArchiveFS could not
safely read it. Neither state is silently called database corruption. SQLite's
extended `SQLITE_READONLY_ROLLBACK` result is reported as
`rollback_recovery_required`, with preservation and copy-first guidance.

For a manual process check, use read-only commands such as:

```sh
pgrep -a archivefs
lsof ~/.local/share/archivefs/*
fuser ~/.local/share/archivefs/*
```

Close an active ArchiveFS process cleanly through its normal UI or shell before
copying database files. Do not kill it merely to make a lock disappear.

## What sidecars mean

- `library.sqlite3-journal` is a rollback journal. SQLite may use it to roll
  back an interrupted transaction. Its existence alone does not establish that
  it is hot, stale, corrupt, or safe to delete.
- `library.sqlite3-wal` holds committed or pending pages in write-ahead-log
  mode. It can contain data absent from the main file.
- `library.sqlite3-shm` coordinates WAL readers and writers.

Never delete, rename, truncate, or edit these files casually. Treat the main
database and every sidecar as one preservation set.

## Interrupted scans

ArchiveFS scans write the catalogue in short, durable SQLite transactions.
`library-scan`, per-source scans, Scan All, and the GUI's Scan Library action
are the production paths that update the `archives` table. If the process is
forcibly terminated while SQLite is committing one of those transactions,
SQLite deliberately leaves a hot rollback journal so the next write-capable
open can restore the pre-transaction pages. This is correct durability
behaviour, not corruption. A normal error or panic unwinds the Rust
transaction and rolls it back; an uncatchable `SIGKILL`, power loss, or host
crash cannot run application cleanup.

The GUI retains its database-scan and source-action worker handles. A second
database action cannot replace an in-progress scan, and normal GUI shutdown
waits for those workers so it does not deliberately abandon a catalogue write.
The CLI does not claim to convert an uncatchable termination into a graceful
shutdown. `SIGKILL`, process abort, host failure, and power loss can still
interrupt SQLite; the rollback journal is precisely the durability mechanism
for those cases.

Do not kill ArchiveFS during a scan. If a hot journal appears, first establish
that no writer remains, preserve the complete file set, and rehearse SQLite's
normal rollback on a copy. Read-only commands never recover a genuinely hot
journal because recovery itself must write.

## Full-set backup

First confirm that no process has any file open. If a process does, do not copy;
close it cleanly, recheck, and rerun read-only diagnostics. With no active user,
create a timestamped directory such as:

```text
~/Backups/ArchiveFS/database-recovery-YYYYMMDD-HHMMSS/
```

Copy the main file and every related `-journal`, `-wal`, `-shm`, temporary,
backup, and migration-related file together. Preserve modes and timestamps
(`cp -a` on Linux is suitable). Verify the copied filenames, byte sizes,
permissions, timestamps, and cryptographic checksums against the originals.
Never make the backup the live database and never alter the originals during
diagnosis.

## Decision tree

### A. An active database user exists

Do not touch sidecars or copy a changing set. Close the process cleanly, recheck
with `lsof`/`fuser`, then rerun `database-check` and read-only SQLite checks.

### B. The database is healthy and a rollback journal remains

If `database-check` reports `zeroed_non_hot` or `truncated_non_hot`, preserve
the evidence if investigating but do not mistake the retained file for pending
recovery. If it reports a hot candidate, a malformed/unreadable header, or
SQLite says recovery is required, preserve the full set first.
Do not simply delete the journal. Duplicate the complete set into a disposable
working directory and let SQLite evaluate/recover that copy through a
controlled write-capable open. Re-run integrity checks on the copy before
considering any further action.

### C. WAL/SHM remain

Preserve the main file, WAL, and SHM together. Do not remove WAL or SHM
manually. Test any recovery or checkpoint only on a copy; a copied main file
without its WAL can be incomplete.

### D. An integrity check fails

Preserve the originals and work only on a copy. Record the exact SQLite result.
On the copy, a qualified recovery workflow may try a normal dump first and
SQLite's recovery tooling only when needed. Write results to a new database and
validate them; never overwrite the original during diagnosis.

### E. Permissions or ownership mismatch

Compare the directory, main file, and sidecar owner/group/mode with the user
running ArchiveFS. Propose the narrowest specific correction. Do not recursively
`chmod` or `chown`, and do not apply any permission change during diagnosis.

### F. Migration failure

Record `PRAGMA user_version`, the ArchiveFS build, and the named failing
migration. Reproduce the migration against a complete copy, fix and test the
code, and only then plan a live retry. A read-only diagnostic never migrates.

## Permission investigation

Inspect the directory and each relevant file with `stat` and `file`. A readable
main file is not sufficient for normal write operation: SQLite may need the
directory and sidecars to be writable by the same user. Report exact mismatches;
do not infer that every `SQLITE_CANTOPEN` result is a permission problem.

## Non-goals

`database-check` is not a repair command. ArchiveFS provides no `--fix` or
`--repair`, does not delete journals, does not checkpoint WAL, does not vacuum
or reindex, does not change ownership or permissions, does not kill processes,
and does not automatically retry a live migration. Recovery is a deliberate,
copy-first procedure requiring explicit user approval.
