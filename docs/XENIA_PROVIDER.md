# Xbox 360 / Xenia Canary patch provider and adapter

EmuWiz can read a verified Xbox 360 Title ID and Media ID directly from
a real archive, look up matching patches in the maintained
[`xenia-canary/game-patches`](https://github.com/xenia-canary/game-patches)
dataset, and install user-selected patches into an explicitly chosen Xenia
Canary profile's `patches` directory - with preview, backup, journal, and
rollback. The provider (retrieval/normalisation) and the adapter
(installation) are separate components, the same way Dolphin's Gecko
provider and Dolphin adapter are separate. EmuWiz does not start Xenia,
execute a patch, or interpret a write operation at any stage.

## Upstream source

- Repository: `xenia-canary/game-patches` (patches live under `patches/`,
  one file per `{TITLE_ID} - {Title Name}[ (TU...)].patch.toml`).
- Retrieval is bounded and never clones the repository: a single index of
  `patches/*.patch.toml` paths is fetched once (via GitHub's Git Trees API
  at the default branch's exact resolved commit), cached locally with a
  timestamp, and refreshed only on an explicit Fetch/Refresh action or
  after its freshness window expires. Only the files whose filename
  begins with the requested Title ID are then downloaded individually
  (`raw.githubusercontent.com`), each cached immutably by
  `(commit, path)` since content at an exact commit never changes.
- No network request happens during ordinary GUI rendering; only an
  explicit Fetch/Refresh triggers one. Refreshing a fresh cache is
  rate-limited to at most once every 30 seconds. Offline mode uses only
  the cache and returns a clear error if nothing is cached yet.

## Licence and attribution

At the time of writing, `xenia-canary/game-patches` publishes **no
`LICENSE` file** (confirmed via the GitHub API's own license field, which
is `null`). EmuWiz records this honestly in the GUI and in every
provider result rather than asserting terms upstream never declared;
content remains the property of its individual authors pending upstream
clarification. The provider ID, display name, source repository, and
resolved commit are always shown alongside any retrieved patch.

## Identity EmuWiz extracts

Only the unencrypted, uncompressed XEX2 module header is ever read - the
compressed/encrypted module body is never touched, decompressed, or
executed:

- **Title ID** (eight hex characters, e.g. `415607D2`): read from the
  XEX execution-info optional header (`XEX_HEADER_EXECUTION_INFO`,
  offset `+0xC`). Supported containers: a direct `.xex` file, or a ZIP
  containing exactly one `.xex` member (mirroring Dolphin's
  ISO-in-ZIP precedent). A filename token matching the 8-hex-character
  shape is candidate evidence only, never promoted to verified.
- **Media ID** (eight hex characters): read from the same header
  (offset `+0x0`).
- **Module hash**: **never computed**. Xenia's own patches typically
  declare one or more 16-hex-character module hashes
  (`hash = "..."` or an array); computing the matching hash would
  require decompressing/decrypting the module body, which is out of
  scope for a bounded, read-only identity reader. Any patch that
  declares a hash is therefore always treated as **not independently
  verifiable** by EmuWiz - see "Exact vs. partial compatibility"
  below. This is a deliberate, permanent limitation, not a placeholder.

Real Xbox 360 archives in EmuWiz's supported formats are `.zip`/`.7z`/
`.rar` containers (a bare `.xex`/`.iso` is not itself a library archive);
identity extraction opens the container and reads only the one matched
`.xex` member's header.

## Exact vs. partial compatibility

Every file the provider returns for a Title ID is classified strictly,
never from title-name similarity:

- **Incompatible**: the file's own `title_id` does not match the
  archive's verified Title ID, or the file declares one or more Media
  IDs and the archive's verified Media ID is not among them. Never
  selectable, under any circumstance.
- **Exact compatible**: the Title ID matches, and the file declares
  neither a Media ID nor a module hash constraint (or every declared
  Media ID matches and no module hash is declared) - i.e. nothing left
  unverified.
- **Partially verified**: the Title ID matches and nothing is
  contradicted, but the file declares a module hash (almost always) or
  a Media ID EmuWiz could not verify against this exact archive.
  Selectable, but only after an explicit user acknowledgement - the
  same "separate approval, never a casual bypass" shape the shared
  transaction already uses for replacing a different existing file. The
  GUI blocks the whole selection from being applied until this
  acknowledgement is given.

Because EmuWiz never computes a module hash, and upstream patches
almost always declare one, most real candidates will be classified
`PartiallyVerified` rather than `ExactCompatible` - this is expected and
correct, not a bug.

## Multiple files per Title ID

Unlike Dolphin's one-GameSettings-file-per-game model, Xenia's own
dataset legitimately has **multiple files sharing one Title ID** - the
same game across different Title Update (TU) releases often has a
distinct module hash and a separate upstream file (e.g. `"... .patch.toml"`
and `"... (TU3).patch.toml"`). EmuWiz always requires the user to pick
which returned file to work with; it never guesses. The destination
filename is always exactly the chosen file's own upstream filename, so
different TU variants never collide with each other or get merged
together.

## Installation location

Xenia Canary has no single standard install location - on Linux it is
typically a portable folder (containing `xenia_canary.exe` and
`xenia-canary.config.toml`, its own real config filename) run natively,
under Wine, or under Proton. EmuWiz therefore only discovers
**caller-supplied explicit directories**, validated the same way as
every other emulator adapter's profile roots (absolute, non-root,
no symlink in any component). No native path is guessed without real
evidence for one existing on this platform. A profile is only eligible
once `xenia-canary.config.toml` is confirmed present as a real,
non-symlink, regular file.

The managed destination is always `<profile>/patches/<file name>`,
mirroring Xenia's own real layout (`patches_root / "patches" / *.patch.toml`,
per `xe::patcher::PatchDB`). EmuWiz never modifies
`xenia_canary.exe`, Xenia's own configuration files, game archives,
Title Updates, or content directories - only this one selected
`patches` destination.

## Apply and rollback guarantees

Reuses the same journal-backed shared transaction engine as every other
write-capable adapter:

- The `patches` directory is created only when needed (a missing
  directory is a valid starting state, not an error).
- Only the exact chosen destination `.patch.toml` is ever created or
  updated; every other file already in `patches` is left untouched.
- When the destination already exists, every patch definition in it that
  is **not** part of the chosen candidate's own patch set is preserved
  completely unchanged (name, description, author, and every write
  entry, byte for byte in content if not in exact original formatting).
  Only `is_enabled` for the chosen candidate's own patches reflects the
  user's selection; deselecting a previously-enabled patch turns it off,
  it does not delete the definition.
- Writes are atomic (temp file + rename) and the exact written bytes are
  verified after writing.
- A replacement backs up the previous file first; rollback restores the
  exact previous bytes. A newly created file is removed on rollback, and
  a `patches` directory the transaction itself created is removed only
  if it ends up empty - a directory that already existed, or that still
  holds other files, is never removed.
- Repeated rollback of the same journal is safe: a second attempt is
  reported as already rolled back, not re-applied or corrupted.

## Unsupported cases (explicit scope boundary for this milestone)

- PS3/RPCS3, original Xbox/Xemu, PCSX2, RetroArch, and Dolphin are
  unaffected by this work.
- No game downloading, Title Update installation, or DLC handling.
- No Cheat Engine integration, patch creation, or patch porting between
  Title Update / module-hash variants.
- No automatic compatibility guessing from title or filename similarity.
- No module hash computation or verification, ever (see above) - this is
  permanent, not a "not yet implemented" gap.
