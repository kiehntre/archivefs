# Dolphin Cheats & Mods adapter

ArchiveFS can discover local Dolphin user profiles, inspect optional existing
per-game INI files, and retrieve Gecko definitions from the official Dolphin
upstream GameSettings dataset. Provider retrieval and Dolphin installation are
separate components. After an exact verified GameCube identity lookup, selected
definitions can be previewed and installed with backup, journal, verification,
and rollback. ArchiveFS does not start Dolphin, evaluate code, or follow
referenced mod paths.

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
never creates profiles. A confirmed Gecko install may create the one exact
`GameSettings/<GAMEID>.ini` destination and its immediate `GameSettings`
directory when absent; discovery and inspection themselves never do.
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

The shared identity reader now verifies a GameCube or Wii Game ID using a
bounded disc-header read from supported ISO input. This enables exact ID
matching. GameCube revision can enable revision-aware matching; Wii
outer-header revision remains candidate-only. An INI or archive filename Game
ID remains an observation, not verified identity. See
[`SHARED_GAME_IDENTITY.md`](SHARED_GAME_IDENTITY.md).

The shared preview uses the verified Game ID and revision and maps only the
conservative `GameSettings/<GAMEID>.ini` destination beneath the approved
Dolphin root. Existing different content requires backup and explicit
replacement permission. A missing file is a valid new-file destination, not a
discovery prerequisite. Texture-pack preview is not supported. See
[`SHARED_CHEAT_PREVIEW.md`](SHARED_CHEAT_PREVIEW.md).

## External Gecko provider

This milestone uses exactly one provider: the maintained
`dolphin-emu/dolphin` repository's structured
`Data/Sys/GameSettings/<GAMEID>.ini` dataset. It was chosen because GAFE01 is
present with a complete Gecko body, the format is already parsed by Dolphin,
anonymous HTTPS retrieval is supported, and the repository is licensed
GPL-2.0-or-later. ArchiveFS shows the source URL, attribution, licence,
retrieval time, exact game ID, encoded region, and revision warning.

The GAFE01 dataset currently supplies `16:9 Widescreen` with five complete
code lines. Upstream does not declare per-entry disc-revision applicability,
so ArchiveFS labels that uncertainty rather than claiming revision-0 proof.
Wrong-region IDs, mismatched response identities, explicitly wrong revisions,
malformed bodies, and ambiguous duplicate names are blocked.

Retrieval uses an ArchiveFS User-Agent, a 15-second overall timeout, a 256 KiB
response bound, and a 30-second minimum refresh interval. Parsed results are
cached locally for 24 hours. Refresh is explicit; rendering never initiates a
request. A validated stale cache remains usable when refresh fails, with the
failure shown. Remote content remains inert text and remote URLs are never
treated as local paths.

## Privacy, safety, and future work

Profile inspection is local. The external request contains only the already
verified six-character Game ID in the provider URL. ArchiveFS does not upload
archive filenames, ROM content, local paths, hashes, or profile metadata. It
has no process-execution path, and original Dolphin files remain untouched
during discovery, retrieval, inventory, and preview. Structural inspection is
not antivirus scanning and does not prove that a cheat or patch is benign.

The Gecko workflow supports individual code selection, preview, explicit
apply, verified backup, journaling, and rollback for existing or missing exact
ID Game INIs. It preserves unrelated settings and Gecko entries and updates
`[Gecko]` and `[Gecko_Enabled]` without duplicate definitions. General section
editing, Action Replay installation, Wii provider routing, and referenced
Riivolution asset inspection remain future work.
