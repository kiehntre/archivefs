# Shared verified game identity

ArchiveFS can inspect narrowly defined, local disc metadata for a selected
PlayStation 2, GameCube, or Wii archive. The result is a typed evidence report,
not a guessed game name. It is shown asynchronously in Cheats & Mods and can
feed exact matching in the existing PCSX2 and Dolphin read-only adapters.

## Evidence states

Every value has one of these explicit states: `Verified`, `Candidate`,
`Missing`, `Unsupported`, `Deferred`, `Invalid`, `Ambiguous`, or
`Resource limit reached`. Provenance records the exact `PathBuf`, optional raw
ZIP member-name bytes and member index, extraction method, confidence, and a
diagnostic. Non-UTF-8 archive paths remain exact OS paths.

Catalogue platform and archive-filename tokens are candidates. They are never
verified identity and the matcher-facing API returns only `Verified` evidence.

## Supported input

- A direct, regular, non-symlink ISO file.
- A regular, non-symlink ZIP containing exactly one unencrypted ISO member.
  GameCube/Wii inspection decompresses only the 32-byte header. PS2 inspection
  buffers at most the first 64 MiB of the ISO member because the existing ZIP
  API is sequential.

CHD, CSO, RVZ, WBFS, 7z, RAR, multiple-ISO ZIPs, encrypted ZIPs, and extracted
directory trees are deferred or unsupported. ArchiveFS has no existing safe,
bounded image API for them in this milestone. It does not invoke `chdman`,
`7z`, a mount helper, PCSX2, Dolphin, or any other process.

## Extracted identities

For GameCube and Wii, ArchiveFS reads bytes `0x00..0x20`, validates the
platform-specific magic (`0x1c` for GameCube or `0x18` for Wii), and accepts a
six-byte Game ID only when every byte is an uppercase ASCII letter or digit.
Game ID, disc number, and raw region-code byte are verified. GameCube revision
is verified from byte `0x07`. Wii outer-header revision remains a candidate:
Dolphin can use revision metadata in the game-partition header, which this
milestone does not decrypt or inspect.

For PlayStation 2, ArchiveFS reads the ISO 9660 primary volume descriptor and
root directory, finds `SYSTEM.CNF`, parses exactly one `BOOT2` assignment, and
accepts only a `cdrom:` or `cdrom0:` path with no empty, dot, or traversal
component. The product code is derived from that exact executable name and is
verified structured metadata. The executable is then resolved through the ISO
directory rather than guessed. When its complete size is within the bound and
it has an ELF signature, ArchiveFS calculates PCSX2's reviewed executable CRC:
XOR of every complete little-endian 32-bit word; trailing one to three bytes
are ignored, matching PCSX2. Otherwise CRC evidence is missing, invalid,
deferred, or resource-limited, never guessed.

## Deterministic limits

| Resource | Limit |
|---|---:|
| Identity payload bytes read per source | 64 MiB |
| ZIP members inspected | 4,096 |
| Metadata path components/lookups | 32 |
| ISO directory entries per lookup | 4,096 |
| ISO volume descriptors | 32 |
| ISO directory bytes per lookup | 1 MiB |
| Path or member-name length | 512 bytes |
| `SYSTEM.CNF` size | 64 KiB |
| Boot executable size | 32 MiB |
| Nested-container depth | 1 |
| Retained warnings | 64 |

ZIP central-directory parsing is delegated to the repository's existing
`zip` library; the 64 MiB value bounds identity payload decompression and all
direct-ISO metadata/executable reads. Any bound that prevents a conclusion is
reported explicitly and cannot produce verified evidence.

## Matching and GUI behavior

A verified Dolphin Game ID enables exact GameSettings INI matching. A verified
GameCube revision can additionally enable revision-aware matching; Wii outer
revision cannot. A verified PCSX2 executable CRC enables exact PNACH matching.
Verified PS2 serial remains supporting evidence because the PNACH adapter's
exact-match contract is CRC-based.

Cheats & Mods displays the selected archive, platform context, evidence state
and value, source method, technical provenance, diagnostics, limits, and the
adapter match result. Disc reads run on a background thread. Results are
accepted only when exact archive path, adapter, page, and platform context still
match the request; superseded receivers are dropped.

## Read-only and privacy guarantees

Production identity code opens local regular files read-only with no-follow
flags, refuses every symlink component, and compares the opened file identity
with the pre-open identity on Unix. It performs no writes, extraction,
temporary copies, mounting, process execution, or network access. It never
executes the boot ELF, uploads an identity or hash, or intentionally changes a
timestamp. All writable synthetic fixtures exist only in unit tests.

Known limitations include sequential prefix-only PS2 access inside ZIP, no
Joliet/UDF reader, no multi-extent/interleaved ISO support, conservative ASCII
identifier validation, no Wii partition decryption, and no direct catalogue
enumeration of ISO files in the existing archive scanner.
