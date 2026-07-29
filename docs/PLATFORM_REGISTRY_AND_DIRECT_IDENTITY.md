# Platform registry and direct game identity

This document covers the canonical platform registry, how folder/source
evidence resolves to a platform, which disc/ROM formats ArchiveFS can read
exact game identity from directly (without mounting), and how the Cheats &
Mods workflow turns that into a final, non-spinning state. It extends
[`PLATFORM_LIBRARY_RECOVERY.md`](PLATFORM_LIBRARY_RECOVERY.md) - read that
document first for the original traced root causes.

## Canonical platform registry

There is exactly one platform-name table in the whole application:
`FOLDER_PLATFORM_ALIASES` in `crates/archivefs-core/src/lib.rs`. Every
caller that needs "what platforms does ArchiveFS know about" or "what
canonical platform does this string mean" goes through it, via two
functions:

- `canonical_platform_names() -> Vec<&'static str>` - every canonical
  platform name, deduplicated and sorted. Used by manual platform
  assignment (GUI and CLI), the Sources "Assign platform" menu, and Library
  View platform filters.
- `canonical_platform_for_alias(hint: &str) -> Option<&'static str>` -
  resolves an arbitrary string (a folder name, a manually-typed platform, a
  libretro database directory name) to its canonical platform, or `None` if
  unrecognised or ambiguous.

Neither the GUI nor the CLI nor any other core module maintains a second,
independently-drifting platform list. Where the GUI previously hardcoded
three separate `["GameCube", "Xbox360", "PS2", "PS3", "Xbox", "Unknown"]`
arrays (Sources, Mount, Library), all three now derive from live per-archive
platform data - see "Full platform filtering" below - and the Sources
"Assign platform" menu lists every `canonical_platform_names()` entry, not
a fixed subset.

A separate, narrower `FOLDER_PREFERRED_EMULATOR_ALIASES` table (and its
accessor `platform_preferred_emulator_for_alias`) answers a different
question - "does this folder name suggest a specific emulator/core?" - for
folder names like `fbneo`/`mame`/`fba` that name an emulator rather than
hardware. This is deliberately **not** merged into the hardware-platform
table: `canonical_platform_names()` never returns `"FBNeo"`, and an FBNeo
collection folder still classifies as the `Arcade` hardware platform. The
two tables answer two different questions about the same folder name.

## Alias normalization

`normalize_path_segment` reduces a whole path *component* (one folder name)
to lowercase ASCII-alphanumeric characters only - `"Sharp X68000"` and
`"sharp-x68000"` both normalize to `"sharpx68000"` and therefore resolve to
the same canonical platform without needing a separate table row per
spelling. Matching is always against one whole normalized component, never
a substring - `"xbox"` can never match inside `"xbox360"` because they
normalize to different strings entirely (`"xbox"` vs `"xbox360"`), and a
folder named `"Nes Collection"` cannot accidentally match `"snes"` because
neither is a substring match candidate to begin with.

Two further layers handle real-world collection folder names without
weakening exact matching:

- **Parenthesized suffixes** (region/date annotations) are stripped before
  matching: `"Nintendo - GameCube (2018-08-25 20-44-48) (America)"` becomes
  `"Nintendo - GameCube"` (still normalized and matched exactly afterward).
- **A trailing `YYYY-MM-DD` date pattern** without parentheses is detected
  and stripped the same way, for collection tools that don't parenthesize
  their timestamps.

Both fall back to an exact match on the stripped remainder - there is no
loose substring or fuzzy matching anywhere in this path.

`detect_platform_from_folder_alias_with_match` walks path components from
the *nearest* to the source root, trying each one in turn, so a deeper,
more specific folder always wins over an outer one - `.../sharp-x68000/Sharp
X68000/[Doujin & Homebrew]/Game.zip` matches on `"Sharp X68000"` (the
nearest component that resolves), never on the unrelated
`"[Doujin & Homebrew]"` segment, and would still resolve correctly even if
the outer `sharp-x68000` folder didn't exist.

## Evidence ordering

Applied in exactly this order (`persist_one_folder` in `database.rs`),
strongest first, and a later/weaker source never overwrites an earlier one:

1. **Manual per-archive override** (`Database::set_manual_platform`)
2. **Exact direct-image header identity** (`PlatformProvenance::HeaderIdentity` -
   currently GameCube/Wii `.iso`/`.gcm` magic-byte detection)
3. **Explicit source-folder platform assignment**, only when the archive's
   extension is compatible with that platform (`source_assignment_is_compatible`)
4. **Persisted custom folder alias** (user-defined, via the platform alias
   admin table)
5. **Built-in exact normalized folder alias** (`FOLDER_PLATFORM_ALIASES`)
6. **Bounded filename/path heuristic** (a small, deliberately narrow set of
   known collection-name substrings - not extended by this work)
7. **Unknown**

## Full platform filtering

Library, Mount, and the Sources "Assign platform" menu all derive their
visible platform list from live data instead of a fixed array:

- Library and Mount both call one shared `detected_platform_counts()`
  helper over their own current row/record set: every distinct platform
  string actually present, sorted, with its real count; `Unknown` is
  counted separately and shown last, only when non-zero. A canonical
  platform the registry recognises but that has zero archives never
  appears - it is never hidden because ArchiveFS lacks a cheat adapter for
  it, and never shown as a phantom empty tab either.
- The platform strip uses `egui`'s wrapping layout (`horizontal_wrapped`)
  rather than a fixed-width tab row or a horizontal-scroll-only area, so a
  library with dozens of detected platforms keeps every non-zero platform
  reachable without scrolling past other controls.
- The Sources "Assign platform" menu lists every `canonical_platform_names()`
  entry (scrollable), so assigning a source to Sharp X68000, ZX Spectrum,
  NEC PC-8801, Virtual Boy, Acorn Archimedes, or Arcade works exactly like
  assigning GameCube always did - no special-casing per platform.
- In Cheats & Mods, a platform ArchiveFS recognises but has no cheat
  adapter for shows its own name ("Sharp X68000 recognised - cheat support
  is not available yet"), not a generic message and not Unknown.

## Direct identity capability table

| Format | Visible in Library | Platform classification | Exact identity | Cheats without mounting |
|---|---|---|---|---|
| GameCube/Wii `.iso`/`.gcm` | Yes | Yes (header magic) | Yes - Verified | Yes |
| GameCube/Wii `.rvz` | Yes | Yes (folder/extension) | **Yes** - Verified, from the documented uncompressed `wia_disc_t.dhead` header field | Yes |
| GameCube/Wii `.ciso` (sparse, uncompressed) | Yes | Yes (folder/extension) | **Yes** - Verified, from the first stored block, when present | Yes |
| GameCube/Wii `.gcz` | Yes | Yes (folder/extension) | No - honestly `Deferred` (per-block zlib/LZMA-family decompression has no existing safe bounded reader in this codebase) | No |
| GameCube/Wii `.wbfs` | Yes | Yes (folder/extension) | No - honestly `Deferred` (requires following the WBFS sector-remap table) | No |
| ZIP containing exactly one `.iso` (GameCube/Wii) | Yes | Yes | Yes - Verified, identical bounded 0x20-byte read as direct ISO | Yes |
| `.chd`, `.cso`, `.7z`, `.rar` (any platform) | Yes | Yes | No - honestly `Deferred`/`Unsupported` | No |
| Xbox 360 direct `.xex` | Yes | Yes | Yes - Verified (Title ID/Media ID from the XEX2 execution-info header) | Yes |
| ZIP containing exactly one `.xex` | Yes | Yes | Yes - Verified, same bounded header read | Yes |
| PS2 `.iso` | Yes | Yes | Yes - Verified (serial from `SYSTEM.CNF`, executable CRC) | Yes |
| PS3 folder (`PS3_GAME/PARAM.SFO`) | Not yet | Not yet | Not implemented this milestone | No |
| Mega Drive/SNES loose cartridge ROM | Yes | Yes (trusted platform context required) | Yes - Verified (SHA-256 of the exact bytes) | N/A (no cheat adapter) |

**RVZ exact Game ID extraction is genuinely implemented and tested** (not a
partial/aspirational claim): `inspect_rvz` in `game_identity.rs` reads only
the fixed, always-uncompressed `wia_file_head_t`/`wia_disc_t` header region
documented in Dolphin's own `docs/WiaAndRvz.md` - magic bytes, `disc_type`,
then the embedded `dhead[0x80]` copy of the disc's own first 0x80 bytes at
its fixed offset (`0x58`) - and reuses the exact same validated GC/Wii
header parser (`inspect_dolphin_header`) that direct `.iso` already used, at
a different byte offset. The compressed disc body is never read. A magic or
`disc_type` mismatch is reported `Invalid` (malformed format), never left
pending.

CISO (the uncompressed, sparse-block GameCube/Wii format - distinct from the
unrelated PSP "CISO" compressed format) is handled the same way: the fixed
0x8000-byte header (magic, block size, block-presence map) is read, and if
the disc-header block (block 0) is present, it is read directly with no
decompression - the whole point of this format is that used blocks are
stored as-is, only unused padding blocks are omitted.

`.gcz` and `.wbfs` remain honestly `Deferred`: both require either
decompressing per-block zlib/LZMA-family data (`.gcz`) or following a sector
remap table across the whole file (`.wbfs`) to reliably locate the disc
header, and neither has a safe bounded reader in this codebase yet. They
were previously silently misclassified as `Unsupported` (a bug fixed this
milestone) rather than the more honest `Deferred` every other
not-yet-implemented format already used.

## No-mount cheat discovery

Cheat discovery never requires mounting. `inspect_catalogued_game_identity`
opens the archive file directly (read-only, with symlink-component
rejection) and performs a small number of bounded, offset-based reads -
32 bytes for a direct ISO/GCM/RVZ/CISO disc header, a few dozen bytes for a
ZIP-contained ISO's header, up to 64 KiB for `SYSTEM.CNF`. Mount state is
informational only; it never gates whether identity inspection or the
subsequent Dolphin catalogue lookup runs. This was already true before this
milestone for `.iso`/`.zip`/`.xex`/PS2 `.iso`; RVZ and CISO now join that
list instead of being permanently blocked on an unsupported format.

## Final unsupported states (why nothing spins forever)

Every identity inspection reaches a **final** `GameIdentityReport` on the
first attempt - there was never an actual infinite loop in identity
inspection itself. The bug this milestone fixed was one level up: the
Dolphin cheat workflow's `dolphin_provider_auto_fetch_needed` gate required
a `Verified` Game ID before it would leave `NotLoaded`, and
`dolphin_beginner_status` had no way to distinguish "still waiting for that
first result" from "got a final result, but it was never `Verified`" - both
looked identical (`dolphin_provider` stuck at `NotLoaded`), so the beginner
page showed "Finding compatible cheats" forever regardless of which one it
actually was.

`BeginnerCheatStatus::IdentityUnavailable` is the fix: derived from the same
`GameIdentityReport` the Details panel already reads, it is reached whenever
identity has resolved (`ready_game_identity(workflow)` returns `Some`) but
`verified_dolphin_game_id()` is `None`. Its detail line is built from the
actual `IdentityStatus` recorded against `DolphinGameId` evidence -
`Invalid` → "malformed or unrecognised layout"; `Deferred` → "cannot yet
read an exact Game ID from it without decompressing the full image";
`Missing` → "the disc-header block is not present"; anything else → a
generic "not supported yet." `dolphin_provider_auto_fetch_needed` was also
tightened to require a verified Game ID, so a permanently-undecodable format
stops re-attempting the (harmless but pointless) local-lookup call every
frame once its terminal state is known.

## Why filename guessing is never used for exact cheat matching

`GameIdentityReport` distinguishes `IdentityConfidence::FilenameOnly`
evidence (e.g. a six-character token found in the archive's own filename)
from `IdentityConfidence::ExactBytes` evidence read from the actual disc/ROM
header. Only `IdentityStatus::Verified` evidence feeds the Dolphin catalogue
lookup (`verified_dolphin_game_id()` returns `None` for anything not
`Verified`, regardless of confidence) - a filename-derived candidate is
never promoted to `Verified` no matter how plausible it looks, because a
filename can be wrong, renamed, or ambiguous in ways on-disc bytes cannot.
This is also why RVZ's fallback state explicitly does not fabricate a Game
ID from the filename even though `ZooCube (USA).rvz` clearly *looks like* it
names the game: a look-alike filename is not evidence a cheat can be safely
matched against.

## Source assignment behaviour

Assigning a platform to a source folder (`Database::set_source_platform_assignment`)
is a **preview-then-confirm** action, never silent: the Sources row shows how
many currently-Unknown entries could change on the next rescan
(`unknown_archive_count`) before the assignment is made. It only affects
archives whose extension is plausible for the assigned platform
(`source_assignment_is_compatible` - for `DirectGameImage` archives,
GameCube/Wii accept their known direct/compressed extensions, every other
platform only accepts `.iso`; ZIP/7z/RAR-contained archives are always
compatible, since the assignment only supplies a platform label, not a
format claim). Archives with a stronger, independently-established platform
(manual override or exact header identity) are never overwritten by a
source assignment - see the evidence ordering above.
