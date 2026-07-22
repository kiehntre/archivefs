# Guided RetroArch Cheat Setup

`archivefs retroarch-cheat-setup <catalogue-path>` is the local entry point;
`archivefs retroarch-cheat-setup --source <source-id>` uses a validated trusted
snapshot. Both discover local
RetroArch profiles, resolves their configured cheat directories, matches the
catalogue against the existing ArchiveFS library database, previews every
decision, and delegates approved writes to the existing safe installer.

The installer does not download a catalogue. The separate trusted-source layer
may retrieve and validate one before setup. Setup never modifies `retroarch.cfg`, enables cheats,
change playlists or cores, or touch game archives.

## Syntax and options

```console
archivefs retroarch-cheat-setup <catalogue-path> [options]
archivefs retroarch-cheat-setup --source <source-id> [options]
```

| Option | Meaning |
| --- | --- |
| `--profile <profile-id>` | Select one exact stable ID shown by setup. Display-name matching is not accepted. |
| `--dry-run` | Show the complete preview without prompting or writing. |
| `--yes` | Approve installation without the final confirmation prompt. |
| `--replace-different` | Permit replacement of a different existing cheat file, after a verified backup. |
| `--json` | Emit only the versioned setup result on stdout. Without `--yes`, this is a successful no-write preview. |
| `--database <path>` | Read an explicit current-schema ArchiveFS library database. |
| `--config <path>` | Restrict eligibility to a discovered profile using this exact RetroArch config path. It does not invent an installation around an arbitrary config. |
| `--source <source-id>` | Retrieve or reuse one compiled-in trusted source; mutually exclusive with the local path. |
| `--offline` | With `--source`, make no network request and require a valid cached snapshot. |
| `--force-refresh` | With `--source`, validate a new snapshot before replacing the current pointer. |
| `--expected-sha256 <hash>` | With `--source`, require this archive digest. |
| `--cache-root <path>` | Override the ArchiveFS-owned source cache root. |
| `--max-download-bytes <bytes>` | Lower the built-in source download ceiling. |

Trusted-source preview provenance and cache behavior are documented in
[`RETROARCH_CHEAT_SOURCES.md`](RETROARCH_CHEAT_SOURCES.md). Local catalogue
behavior is unchanged.

No destination, journal, backup, installation-type, or RetroArch config path
is needed normally. The destination comes from the selected profile's parsed
`cheat_database_path`. Journals and backups use the same default ArchiveFS
data directory as `retroarch-cheat-install`; `--database` changes only the
read-only game-library input.

## Profile discovery and selection

Setup reuses `retroarch-environment` discovery for native, Flatpak user/system,
and verified distinct-config AppImage/portable profiles. An AppImage sharing
the native config remains part of the native logical profile rather than
appearing twice.

A profile is eligible only when its installation identity has adequate
executable or Flatpak evidence, its config is a readable complete UTF-8 file,
`cheat_database_path` resolves losslessly, and the existing destination-root
validator finds no traversal, wrong-type, or symlink hazard. A configured
missing cheat directory is allowed: preview does not create it, and the
installer creates only the necessary chain after approval.

One eligible profile is selected automatically and displayed. With several,
an interactive terminal shows type, scope, stable ID, config, and destination,
then requires a numbered choice. There is no default. `q`/`cancel` or
end-of-input cancels successfully.

`--dry-run`, `--json`, and redirected/non-terminal input never prompt. With
several profiles, pass the exact `--profile` ID. Unknown IDs and discovered but
ineligible IDs fail separately. When none are eligible, every discovery and
its blocker codes remain visible.

## Catalogue and database requirements

The local input accepts the same bounded, no-follow formats as the existing
catalogue and installer commands:

- a directory tree of RetroArch `.cht` files, with its immediate child
  directory used as the platform hint; or
- the existing bounded JSON manifest format.

Existing traversal, symlink, size/count, UTF-8, parser, platform, safe-name,
and duplicate-destination rules remain in force. There is no network access,
Git clone, update, or automatic catalogue search.

The database opens read-only and is never created, migrated, repaired, or
scanned by setup. A missing, inaccessible, outdated, or empty database fails
with a corrective hint. Setup does not add source folders or scan the
filesystem.

## Preview and confirmation

The human assistant shows `RetroArch profile`, `Cheat catalogue`, `Match
summary`, `Planned changes`, and `Warnings` where needed. It reports the
profile/config/destination, game and cheat counts, exact/strong and
weak/ambiguous matches, new/identical/different destinations, conflicts,
skips, proposed writes/backups, and prospective journal. Each entry includes
title, platform, source, destination, action, confidence, and reason.

Only exact or strong, complete, unambiguous matches can be actionable. Weak
and ambiguous matches never become installs. Different existing files show as
`skipped` unless `--replace-different` is present.

Without `--yes`, an interactive human run asks for the literal confirmation
`yes` only when a write is proposed. Rejection is a successful `cancelled`
result with no directory, backup, or journal. Zero-write previews do not
prompt and explain their likely cause. `--dry-run` never prompts or writes.
`--json` without `--yes` is a successful `preview`, preserving the established
no-`--yes` preview behavior.

```console
# Human-readable no-write preview
archivefs retroarch-cheat-setup ~/cheats --dry-run

# Deterministic JSON preview
archivefs retroarch-cheat-setup ~/cheats \
  --profile native-user-0123456789abcdef --json

# Interactive or non-interactive installation
archivefs retroarch-cheat-setup ~/cheats
archivefs retroarch-cheat-setup ~/cheats \
  --profile native-user-0123456789abcdef --yes

# Verified backup and replacement
archivefs retroarch-cheat-setup ~/cheats --yes --replace-different
```

## Installation safety

Setup contains no copying or replacement implementation. After approval it
passes the existing `CheatAvailabilityEntry` plan unchanged to
`execute_cheat_install_run`. That installer remains responsible for immediate
source-hash and destination-state revalidation, lossless destination
reconstruction, duplicate defence, symlink refusal, temporary files and atomic
renames, verified backups, post-write hashing, per-entry failures, and the
immutable journal. A source or destination changed after preview is refused.

Discovery and preview create no destination, journal root, backup root, or
other directory. They do not modify RetroArch config, catalogue, database,
games, playlists, or cores.

## JSON result

The result has `schema_version: 1` and a lower-snake-case `status` of
`preview`, `cancelled`, `applied`, or `failed`. It includes the selected
profile, every discovery and eligibility blocker, config/destination and
catalogue/database paths, preview summary, planned entries, installer result
when applied, produced journal, warnings, errors, and structured `next_steps`
with command argument arrays.

Paths use the existing encoded shape. A `lossy: true` config or destination is
descriptive only and makes the profile ineligible; it is never reconstructed
as a security identity. JSON never prompts or mixes human prose into stdout.

## Using installed cheats in RetroArch

1. Start the matching game in RetroArch.
2. Open **Quick Menu**.
3. Open **Cheats**.
4. Use **Load Cheat File** or the equivalent loading action.
5. Select the matching file if RetroArch did not load it automatically.
6. Enable the individual entries wanted.
7. Use **Apply Changes**.
8. Optionally use supported auto-apply or game-specific cheat-save settings.

Installing a file does not enable its cheats. Game identity, region, revision,
emulator core, and cheat format can affect compatibility.

Completion prints the actual journal and follow-up commands:

```console
archivefs retroarch-cheat-history
archivefs retroarch-cheat-inspect ~/.local/share/archivefs/cheat-install-runs/<run>.json
archivefs retroarch-cheat-rollback ~/.local/share/archivefs/cheat-install-runs/<run>.json \
  --cheat-destination-root /resolved/profile/cheats --dry-run
```

Review rollback, then add `--yes` only when its assessments are expected.

## Limitations and troubleshooting

- **No RetroArch found:** launch an installed RetroArch once; ensure its
  executable or Flatpak metadata remains discoverable.
- **Multiple profiles:** use the intended exact `--profile` ID. Similar paths
  are never used to merge or guess.
- **Unresolved cheats directory:** set an absolute `cheat_database_path` in
  RetroArch. Empty values, relative syntax, aliases, and includes are not
  guessed or rewritten.
- **Missing database:** run the normal ArchiveFS source/library scan. Setup
  never creates or migrates it.
- **No matches:** verify that the game is present and catalogue title/platform
  layout corresponds to it.
- **Weak matches:** add stronger local identity/platform evidence; `--yes`
  cannot promote weak evidence.
- **Replacement blocked:** inspect the existing file, then add
  `--replace-different` only when backup and replacement are intended.
- **Flatpak paths:** user config normally lives below
  `~/.var/app/org.libretro.RetroArch/config/retroarch/`; native and Flatpak
  remain distinct profiles.
- **AppImage/portable ambiguity:** use a resolved RetroArch `-c/--config`
  launcher or AppImage portable config. Conflicting evidence is ineligible.
- **Unsupported layouts:** setup does not guess runtime defaults, arbitrary
  portable layouts, or an installation from a manually supplied unverified
  config alone.

No GUI is added in this milestone. The reusable core result and orchestration
models allow a future GUI to avoid parsing CLI text.
