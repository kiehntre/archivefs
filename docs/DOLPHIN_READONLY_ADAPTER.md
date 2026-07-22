# Read-only Dolphin Cheats & Mods adapter

ArchiveFS can discover local Dolphin user profiles and inspect their existing
per-game INI files from the Cheats & Mods workspace for GameCube and Wii
archives. The adapter is observational only. It does not start Dolphin,
evaluate cheat codes or patches, follow referenced mod paths, download
content, or create, copy, modify, enable, disable, rename, delete, sanitize, or
generate any Dolphin file.

## Discovery

Default Linux discovery checks only documented user locations:

- native/XDG: `$XDG_CONFIG_HOME/dolphin-emu` (or
  `~/.config/dolphin-emu`);
- Flatpak user profile:
  `~/.var/app/org.DolphinEmu.dolphin-emu/config/dolphin-emu`;
- Flatpak user and system installation markers, used only to describe profile
  scope; a system Flatpak still uses a per-user configuration;
- an exact user directory supplied by another trusted component. ArchiveFS
  never searches arbitrary locations for portable profiles.

A profile must be an absolute, non-root path with no symlink in an existing
component, and `Dolphin.ini` at its root must be a regular, non-symlink file.
Existing unsafe or unproven candidates remain visible as blocked. Missing
standard candidates are ignored; missing explicit roots are blocked. ArchiveFS
creates neither profiles nor `GameSettings`.
Unix device/inode identity is captured during discovery and checked before
inventory. Exactly one eligible profile may be selected automatically;
multiple eligible profiles require an explicit choice.

## Game INI inspection

Only regular, lowercase `.ini` entries immediately within `GameSettings` are
opened. Directories are not recursively searched. Symlinks and special files
are reported and never followed; Unix opens use `O_NOFOLLOW`. Entries are
sorted by their original OS paths, preserving non-UTF-8 filesystem identity.

The parser treats all content as inert text. It records:

- filename Game ID, optional `r<revision>`, and region candidates;
- names declared by `[OnFrame]`, `[ActionReplay]`, `[Gecko]`, and
  `[Riivolution]`;
- names referenced by the corresponding `_Enabled` sections;
- byte size, SHA-256, parse warnings, and duplicate identity, filename, or
  content observations.

Unknown sections and ordinary code-data lines are ignored. Malformed section
or code-name syntax is warned about. ArchiveFS does not validate that a code is
correct, safe, compatible, or actually active in Dolphin.

## Fixed resource limits

- 16 profiles;
- 10,000 `GameSettings` entries visited;
- 2,048 Game INI files;
- 256 KiB per Game INI;
- 16 MiB total Game INI input;
- 8,192 lines per file;
- 8 KiB per line;
- 128 retained names per supported section kind.

Limit exhaustion makes the inventory explicitly incomplete. The GUI renders
at most 100 file cards and 50 warning lines while retaining the bounded core
result.

## Identity and matching

The core matcher accepts a Game ID and optional revision only when its caller
supplies those values as separately verified archive evidence. It distinguishes
one exact ID match, an exact ID-and-revision match, multiple matching files, a
revision mismatch, invalid input, and no match.

The current GUI has no reviewed, bounded GameCube/Wii disc-header identity
reader, so it reports verified identity extraction as unavailable. An INI
filename Game ID is an observation about that INI, not proof that the selected
archive is the same game. Future exact matching requires a separately reviewed
reader for supported archive formats that does not mount, extract, or execute
content.

## Privacy, safety, and future work

All profile inspection is local. No filename, content, hash, result, or
metadata is uploaded. The adapter exposes no network or process-execution path,
and original Dolphin files remain untouched. Structural inspection is not
antivirus scanning and does not prove that a cheat or patch is benign.

Preview, installation, conflict handling, verified backup, journaling,
rollback, enabling, disabling, and referenced Riivolution asset inspection are
future work. The GUI exposes none of those actions in this read-only milestone.
