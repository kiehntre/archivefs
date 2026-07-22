# RetroArch Cheat Rollback

Before performing a rollback, the read-only
[`retroarch-cheat-history` and `retroarch-cheat-inspect`](RETROARCH_CHEAT_HISTORY.md)
commands can show whether the destination and backup still satisfy its safety
preconditions and whether a matching rollback journal already exists.

Rollback reverts one successful `retroarch-cheat-install` run identified by
its journal. It never guesses a "latest" run and never overwrites files that
changed after installation.

The same command consumes journals produced by guided
[`retroarch-cheat-setup`](RETROARCH_CHEAT_SETUP.md). Setup completion prints the
actual journal and resolved destination root in a ready-to-preview rollback
example.
Trusted-source installs retain the same local hash, backup, destination,
journal-binding, and confirmation checks; retrieval provenance does not weaken
rollback safety.

```console
archivefs retroarch-cheat-rollback ~/.local/share/archivefs/cheat-install-runs/<run>.json \
  --cheat-destination-root ~/.config/retroarch/cheats --yes
archivefs retroarch-cheat-rollback /tmp/install.json \
  --cheat-destination-root /tmp/retroarch-cheats --dry-run --json
```

`--cheat-destination-root` is required. Without `--yes` the command is a
preview; `--dry-run` always suppresses writes, even with `--yes`. A real run
writes one immutable journal below
`$XDG_DATA_HOME/archivefs/cheat-rollback-runs/` (or the platform default
`~/.local/share/archivefs/...`).

Entries recorded as `installed_new` are removed only when the destination is
still a safe regular file whose hash equals the hash installed by ArchiveFS.
Missing files are `already_restored`; changed files are preserved and reported
as `failed_destination_changed`.

For `replaced_with_backup`, rollback verifies the destination replacement hash,
the backup's previous-content hash, and that the backup is beneath the
ArchiveFS backup root with no symlink components. The verified bytes are
written through a same-directory temporary file, fsynced, hash-checked, and
atomically renamed into place. Backups remain available after success. A
destination already matching the backup is `already_restored`; missing or
modified backups are refused.

`already_installed` and all non-writing install outcomes produce
`no_change_required`. Unsafe, absolute, traversal, symlink, wrong-type, and
malformed paths are rejected. Each entry is independently revalidated, so one
failure does not stop unrelated entries; partial success is recorded honestly.
The JSON result includes stable outcome codes, hashes, write status, summary,
and any journal-write error. A journal-write failure is non-zero and does not
hide filesystem actions that already occurred.

Rollback cannot defeat a race occurring after its final safety check (the same
documented residual TOCTOU limitation as installation); users should avoid
concurrent edits to the destination and backup trees.
