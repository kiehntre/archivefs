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

Preserve the full set first. Do not simply delete the journal. If recovery is
needed, duplicate the complete set into a disposable working directory and let
SQLite evaluate/recover that copy through a controlled write-capable open.
Re-run integrity checks on the copy before considering any further action.

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
