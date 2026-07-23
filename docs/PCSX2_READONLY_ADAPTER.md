# Read-only PCSX2 Cheats & Mods adapter

ArchiveFS can discover local PCSX2 profiles and inspect their existing PNACH
files from the Cheats & Mods workspace. This adapter is observational only. It
does not start PCSX2, evaluate patch directives, download a patch collection,
or create, copy, modify, enable, disable, rename, delete, sanitize, or generate
any PCSX2 file.

## Discovery

The default Linux discovery checks only documented locations:

- native/XDG: `$XDG_CONFIG_HOME/PCSX2` (or `~/.config/PCSX2`);
- Flatpak user profile:
  `~/.var/app/net.pcsx2.PCSX2/config/PCSX2`;
- Flatpak user and system installation markers, used only to describe scope. A
  system Flatpak still has a per-user configuration;
- portable/AppImage roots only when another trusted component supplies an
  exact, already-known configuration root. ArchiveFS does not search arbitrary
  locations or infer portable roots from filenames.

A profile must use an absolute, non-root path; every existing path component
must be a real directory rather than a symlink; and the configuration must
contain a no-follow `inis` directory or `PCSX2.ini` marker. Existing unsafe or
unproven candidates remain visible as blocked. Missing standard candidates are
ignored and missing explicit portable roots are reported as blocked. ArchiveFS
never creates a missing profile or patch directory. On Unix, device/inode
identity is captured during discovery and checked again before inventory.

Exactly one eligible profile may be selected automatically. Multiple eligible
profiles require an explicit choice.

## Directories and categories

- `cheats` — Cheats;
- `cheats_ws` — Widescreen patches;
- `patches` — Other PNACH patches, only when that directory exists.

Category is inferred only from the directory. Missing directories are normal.
Symlinked, unreadable, non-directory, and changed paths are reported and never
followed.

## PNACH parsing

Only regular `.pnach` files are opened. Unix opens use `O_NOFOLLOW`. Directory
entries are sorted by their original OS paths, so non-UTF-8 paths remain valid
filesystem identities even when the GUI needs a lossy display label.

The parser records filename and CRC/serial candidates, `gametitle=` and
`region=` text, enabled/disabled/unknown patch syntax counts, directory-derived
category, size, SHA-256, and duplicate CRC/filename/content observations.
Directives are counted as text only and never executed or evaluated. Malformed
text produces warnings. Comments and filename CRCs do not prove game identity.

## Fixed resource limits

- 16 profiles;
- 4 supported patch directories per profile;
- 256 directories traversed;
- 10,000 filesystem entries visited;
- depth 4 beneath each patch root;
- 2,048 PNACH files;
- 256 KiB per PNACH file;
- 16 MiB total PNACH input;
- 8,192 lines per file;
- 8 KiB per line.

Limit exhaustion makes the inventory explicitly incomplete. The GUI renders at
most 100 file cards and 50 warning lines while retaining the bounded core
result.

## Identity and matching

The core matcher reports an exact match only when its caller supplies a
separately verified PCSX2 executable CRC. It distinguishes one exact match,
multiple files for that CRC, and no match. Without a verified CRC, a
conservative comment-title equality is only an unverified candidate.

The shared identity reader can now verify a PS2 serial from `SYSTEM.CNF` and
calculate PCSX2's executable word-XOR CRC from the complete, exactly resolved
boot ELF in a supported ISO. Only that verified CRC enables exact PNACH
matching. ZIP prefix limits or an unavailable image format leave CRC explicit
as deferred or resource-limited. See [`SHARED_GAME_IDENTITY.md`](SHARED_GAME_IDENTITY.md).

## Privacy, safety, and future work

All inspection is local. No filenames, contents, hashes, results, or metadata
are uploaded. The adapter has no network or process-execution path. Original
PCSX2 files remain untouched. Structural inspection is not antivirus scanning
and does not prove that a patch is benign or correct.

Preview, installation, conflict handling, verified backup, journaling,
rollback, enabling, and disabling are future work. The GUI exposes none of
those actions in this read-only milestone.
