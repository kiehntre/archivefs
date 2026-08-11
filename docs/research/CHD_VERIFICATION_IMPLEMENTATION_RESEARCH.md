# CHD-Aware DAT Verification: Research & Architecture

Status: research only. No application code was changed for this document. Repo `kiehntre/emuwiz`. Researched against `origin/main` at `f7c450c` (merge of PR #29); every repository file:line citation below was re-verified against current `origin/main` at `7c8d6ea` (merge of PR #33) and remains accurate — PR #33 changed only desktop/icon/install/release areas, none of which this report cites. Companion document: `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md`, which defines the general verification pipeline (outer file/container → inspect payload/member → raw verification → optional platform-specific normalization → exact DAT match → provenance-rich result). ZIP is the first container P0; NES header normalization is the first platform normalizer. This document answers: where does CHD belong in that architecture, and how is CHD verification made safe without rewriting user files?

Tagging key: **DOCUMENTED FACT** (external, cited source), **CONCLUSION FROM SOURCE** (file:line in this repo), **INFERENCE** (reasoning stated), **UNCERTAIN** (flagged explicitly, not guessed).

---

## 1. Current codebase audit — exact integration points

### 1.1 Where `.chd` appears today (extension-only, no reader)

- `crates/archivefs-core/src/inspector.rs:120` — `"chd"` is in `LIKELY_CONTENT_EXTENSIONS` ("Disc-image extensions" group). `classify_entry` (inspector.rs:132-153) therefore labels a `*.chd` *entry inside a ZIP* as `LikelyContent`. The entry's bytes are never opened. **CONCLUSION FROM SOURCE**: the Inspector is ZIP-only and metadata-only (`is_inspectable` is `matches!(archive_kind(path), Some(ArchiveKind::Zip))`, inspector.rs:271-273); a `.chd` path is not inspectable and `inspect_archive_with_limit` returns `UnsupportedFormat` before `File::open`.
- `crates/archivefs-core/src/game_identity.rs:513` — dispatch arm: `"chd" | "cso" | "rvz" | "wbfs" | "ciso" | "gcz" | "7z" | "rar"` → `report.format = IdentityImageFormat::Deferred;` and evidence `IdentityStatus::Deferred`, message `"format has no existing safe bounded reader in EmuWiz"`. **CONCLUSION FROM SOURCE**: `.chd` is explicitly *deferred*, not *unsupported*, in identity inspection, and is never opened (no read, no header parse, no hash). `IdentityStatus` (game_identity.rs:121-130) already has `Deferred` ("Not available yet") as a distinct, honest state.
- `crates/archivefs-core/src/platform/mod.rs:146` — `"chd"` in `SHARED_EXTENSIONS` ("must never identify one on its own"), and in `weak_extensions` for 18 platforms (3DO:279, Amiga CD32:324, Arcade:400, Commodore CDTV:631, FM Towns:657, NeoGeo:809, Neo Geo CD:835, PC Engine/SuperGrafx:1094, TurboGrafx CD:1115, Philips CD-i:1134, Sega Dreamcast:1208, Sega Mega Drive:1289, Sega CD/Mega CD:1314, Sega Saturn:1338, Sony PlayStation:1381, PlayStation 2:1398, Sony PSP:1433). Arcade explanation (405): *"Arcade sets are `.zip`/`.chd` files whose names are the only identification."*
- `crates/archivefs-gui/src/main.rs:48400, 48443, 48467` — GUI tests feed `Path::new("/missing.chd")` into `inspect_game_identity` for PS2/GameCube to exercise stale-result rejection. No CHD logic.
- `chdman` appears nowhere in the repository (**CONCLUSION FROM SOURCE**, `rg` across all crates).

### 1.2 Where CHD is NOT yet a first-class citizen

- `archive_kind` (`lib.rs:3296-3327`): `[".iso", ".gcm", ".gcz", ".rvz", ".wbfs", ".ciso"]` → `ArchiveKind::DirectGameImage`. **`.chd` is absent.** A `.chd` file is not recognised as any archive kind, so it is not catalogued as a game image, is not identity-inspected (it never reaches `inspect_game_identity` as a DirectGameImage), and is not watched/imported as one.
- `watch_path_is_supported_archive` (`lib.rs:5556-5559`): `"zip" | "rar" | "7z" | "iso" | "md" | "gen" | "smd" | "bin"` — `.chd` absent.
- `database.rs:3995-4010` `source_assignment_is_compatible`: for `DirectGameImage`, GameCube accepts `iso|gcm|gcz|rvz|ciso`, Wii accepts `iso|gcz|rvz|wbfs|ciso`, everything else only `iso`. No `chd`.

### 1.3 DAT audit pipeline (what a CHD would encounter today)

- `run_dat_audit` (`dat/sources/audit_run.rs:269-483`) walks a folder with `scan_local_files` (audit_run.rs:649-732; `MAX_SCAN_DEPTH=8`, `MAX_SCAN_FILES=25_000`, `MAX_SCAN_ENTRIES_EXAMINED=200_000`), hashes **every regular file** — including `.chd` — via `hash_file_reporting` (audit_run.rs:411; `identity_source/hashing.rs:288`), which is chunked (256 KiB), cancellable, and bounded by `MAX_AUTOMATIC_HASH_BYTES = 8 GiB` (hashing.rs:55). **CONCLUSION FROM SOURCE**: today a `.chd` is treated as an opaque byte blob — its CRC32/MD5/SHA-1 of the *container bytes* are computed and compared against the DAT index. This is meaningless for any DAT that publishes logical/track/CHD-SHA1 identities, and it is the exact "don't assume CHD behaves like ZIP" trap: a CHD is a container whose raw bytes are *not* the identity.
- `audit_one` (`dat/audit.rs:200-297`) ladder: SHA-256 → SHA-1 → MD5 → CRC32+size → filename. `DatIndex` (index.rs:29-36) keys crc32/md5/sha1/sha256/filename. **CONCLUSION FROM SOURCE**: the index can already hold SHA-256; `hash_file_reporting` does not yet compute SHA-256 (`LocalHashes` has crc32/md5/sha1 only, hashing.rs:156-160). A CHD adapter that needs SHA-256 (e.g. a logical-image hash) must either compute it in the adapter or extend `LocalHashes`.
- DAT parsers (`dat/parsers/logiqx.rs`, `clrmamepro.rs`): parse `<rom>` entries with `name/size/crc32/md5/sha1/sha256/status/merge/date` (`dat/model.rs:133-143`). **They do not parse MAME software-list `<disk name=... sha1=.../>` elements**, and they have no CHD-specific fields. **CONCLUSION FROM SOURCE**: MAME software-list CHD identities (which live in `<disk>` elements) are *not currently ingestible*; Redump-style DATs parse per-track `<rom>` entries as plain files (see test `redump_disk_records`, logiqx.rs:1110-1136).

### 1.4 Subprocess / external-tool pattern (relevant if chdman is ever used)

- `command_available` (`lib.rs:7203-7212`) — PATH/binary existence probe.
- `run_command_os_with_timeout` (`lib.rs:7225-7283`) — the only generic runner: `Command::new(program).args(args).stdout(piped).stderr(piped).spawn()`, 30 s timeout, kill-on-timeout, output bounded to `COMMAND_OUTPUT_LIMIT = 64 KiB`, maps failures to `ArchiveFsError::ExternalCommand`. Used for ratarmount/fusermount/umount only. **CONCLUSION FROM SOURCE**: there is a single, safe argv-array subprocess abstraction (no shell interpolation), but its output cap (64 KiB) and 30 s timeout are far too small for any CHD `verify`/`extract` path and it discards stdout by default.
- `identity_source/verification.rs` — `VerificationStore` ("verified-hashes.json", `VERIFICATION_FORMAT_VERSION=1`, `MAX_VERIFIED_ENTRIES=20_000`): hash local file (CRC32/MD5/SHA-1) and compare with a provider-published `ExternalHash`, promoting RomM records to `ConfirmedExternal`. **CONCLUSION FROM SOURCE**: the only existing "verify a local file against a published hash" store, and it is per-file + provider-scoped, not a CHD-aware pass.
- `game_identity.rs` has a source-lint test (2693-2716) asserting production identity code contains no `File::create`/`fs::write`/`Command::`/`std::process`/`TcpStream`/`ureq`/`http`. **CONCLUSION FROM SOURCE**: identity reading is deliberately write/process/network-free. A chdman-backed verifier would have to live outside that lint's scope (it already does for mount code) and must be treated as a separate, carefully-contained surface.

### 1.5 What exists vs. what does not (integration-point summary)

| Surface | Exists | For CHD |
|---|---|---|
| extension classification | `LIKELY_CONTENT_EXTENSIONS` (inspector.rs:120) | `LikelyContent` label, no open |
| platform hint | `weak_extensions` × 18 (platform/mod.rs) | present, weak only |
| identity inspection | `IdentityImageFormat::Deferred` (game_identity.rs:513) | deferred, never opened |
| archive kind | `ArchiveKind::DirectGameImage` (lib.rs:3305) | `.chd` absent |
| DAT audit hashing | opaque whole-file CRC32/MD5/SHA1 (audit_run.rs) | present but meaningless for CHD |
| DAT model | `<rom>` crc/md5/sha1/sha256 | no `<disk>` / CHD-SHA1 fields |
| read-only open policy | `safe_read` (TrustedRoots, O_NOFOLLOW) | applicable to the outer file |
| subprocess runner | `run_command_os_with_timeout` | exists, but 64 KiB/30 s caps unfit |
| BIN/CUE pairing | deliberately none (inspector.rs:156-158, 820-841) | n/a |
| CHD reader / chdman | none | **greenfield** |

---

## 2. CHD format model (v5)

Source: MAME `src/lib/util/chd.h` (BSD-3-Clause header comment, read live at commit f7c450c-era master) and `chd.cpp`/`chdman.cpp`.

### 2.1 Header (v5)

**DOCUMENTED FACT** (chd.h V5 header layout), all big-endian, 124-byte header:

| Offset | Field |
|---|---|
| 0 | `tag[8]` = `'MComprHD'` |
| 8 | `length` (u32) = header length |
| 12 | `version` (u32) = 5 |
| 16 | `compressors[4]` (u32×4) — codec ids; `compressors[0]==0` means uncompressed |
| 32 | `logicalbytes` (u64) — logical size of the data |
| 40 | `mapoffset` (u64) |
| 48 | `metaoffset` (u64) |
| 56 | `hunkbytes` (u32) — bytes per hunk (512 KiB max per spec; chdman allows up to 1 MiB) |
| 60 | `unitbytes` (u32) — bytes per unit within a hunk |
| 64 | `rawsha1` (20) — **raw data SHA-1** |
| 84 | `sha1` (20) — **combined raw+metadata SHA-1** |
| 104 | `parentsha1` (20) — combined raw+meta SHA-1 of parent; nonzero ⇒ delta CHD |

### 2.2 Hunks, map, compression

- A CHD is a sequence of **hunks**, each `hunkbytes` of uncompressed data. A hunk is the unit of random access and decompression.
- The **map** (at `mapoffset`) is compressed for compressed CHDs. Each map entry expands to `{ compression (u8), complength (UINT24), offset (UINT48), crc-16 (u16) }`. Special pseudo-codecs: `CHD_CODEC_SELF=1` (copy of another hunk in this file — dedup), `CHD_CODEC_PARENT=2` (copy of a parent hunk/unit), `CHD_CODEC_MINI=3` (legacy 8-byte repeat). **CONCLUSION FROM SOURCE**: a CHD is a deduplicating, self-referential container — hunks may alias each other or the parent, so "the bytes of the file" are never the identity and partial reads must follow the map.
- Compression codecs (chdman docs, "Compression algorithms"): `zlib`, `zstd`, `lzma`, `huff`, `flac` (2ch/16-bit/44.1 kHz PCM), `cdzl` (zlib, CD data+subchannel split), `cdzs` (zstd, CD), `cdlz` (LZMA audio + zlib subchannel, CD), `cdfl` (FLAC audio + zlib subchannel, CD), `avhu` (LaserDisc A/V). Defaults: `createcd` → `cdlz,cdzl,cdfl`; `createdvd`/`createhd`/`createraw` → `lzma,zlib,huff,flac`; `createld` → `avhu`. `zstd`/`cdzs` require newer chdman readers ("older software may not support CHD files that use Zstandard compression").

### 2.3 Metadata (tagged blobs)

**DOCUMENTED FACT** (chd.h tags): `GDDD` hard-disk geometry (`CYLS:%d,HEADS:%d,SECS:%d,BPS:%d`), `IDNT`/`KEY `/`CIS ` (HD identify/key/PCMCIA), `CHCD` (old CD TOC), `CHTR` (`TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d`), `CHT2` (`TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d PREGAP:%d PGTYPE:%s PGSUB:%s POSTGAP:%d`), `CHGT`/`CHGD` (GD-ROM, adds `PAD:%d`), `DVD ` (DVD), `AVAV`/`AVLD` (A/V LaserDisc, `FPS:%d.%06d WIDTH:%d HEIGHT:%d INTERLACED:%d CHANNELS:%d SAMPLERATE:%d`).

Metadata carries the *logical structure* (track layout, disc type) that raw byte hashes cannot. **CONCLUSION FROM SOURCE**: metadata is a first-class, checksummed part of the CHD identity — see 2.5.

### 2.4 What the CD "logical data" stream actually is

**DOCUMENTED FACT** (cdrom.h): `FRAME_SIZE = MAX_SECTOR_DATA + MAX_SUBCODE_DATA = 2352 + 96 = 2448` bytes. CD CHDs are stored as streams of **2448-byte frames** (2352-byte sector + 96-byte subcode), unit size 2448, default 8 frames/hunk (18,816 B). `createcd` **pads each track to a 4-frame boundary** (`TRACK_PADDING = 4`) before writing, and `cdrom.cpp` "deals with this on the read side" (chdman.cpp createcd: `extraframes` per track). Sector types (cdrom.h:44-60): `MODE1` (2048 B), `MODE1_RAW` (2352 B), `MODE2` (2336), `MODE2_FORM1` (2048), `MODE2_FORM2` (2324), `MODE2_FORM_MIX`, `MODE2_RAW` (2352); subcode `CD_SUB_NORMAL`/`CD_SUB_RAW` (96 B) or `CD_SUB_NONE`.

**Critical implication for verification**: `createcd` reads a BIN/CUE, then **stores per-track frame counts with 4-frame padding and a 2448-byte frame size**, plus the track metadata (CHT2). The CHD's `logicalbytes` and its `rawsha1` are therefore over the *padded, 2448-B-frame stream*, **not** over the original `.bin` files and **not** over the original track data as a Redump DAT would hash it. **CONCLUSION FROM SOURCE**: a CHD's raw SHA-1 is an identity of the CHD's internal logical stream, not of the source disc image. This is the core "do not assume a CHD hash can be compared directly with DAT ROM hashes" finding (section 7).

### 2.5 Which hashes identify what (v5)

| Hash (header field) | What it identifies | Directly comparable to |
|---|---|---|
| `rawsha1` | The CHD's uncompressed logical data stream, exactly as stored (CD: padded 2448-B frames; HD/DVD: raw unit stream) | Nothing in a Redump/TOSEC/No-Intro DAT. Matches only itself, or a re-`create`d equivalent stream. |
| `sha1` (overall) | `SHA1( rawsha1 ‖ sorted[ (u32be tag ‖ SHA1(metadata contents)) for checksummed metadata ] )` (compute_overall_sha1, chd.cpp:1709-1751) | **MAME software-list `<disk sha1="...">` values** and any CHD-aware DAT that publishes the CHD's overall SHA-1. This is the standard MAME convention. |
| `parentsha1` | The parent CHD's overall SHA-1 | Locating a parent by its header overall SHA-1. |
| container-file SHA-1 (computed over the whole `.chd` file on disk) | The physical `.chd` file | Nothing meaningful; changes with compression settings/hunk size without changing content. |

**CONCLUSION FROM SOURCE**: there are three different "hashes" for one CHD, and only the **overall SHA-1** is a portable identity that a DAT ecosystem actually publishes. The **raw SHA-1** is an integrity self-check (recomputed by decompressing), never a catalogue key. The **container-file hash** is what EmuWiz's DAT audit computes today and is worthless for CHD.

### 2.6 Parent/child CHDs

**DOCUMENTED FACT** (chd.h): `parentsha1 != 0` ⇒ delta CHD; map entries may be `CHD_CODEC_PARENT` referencing a parent hunk/unit. chdman requires `--inputparent` for any command on a delta CHD. `verify` reports "requires parent" errors when the parent is absent. **CONCLUSION FROM SOURCE**: delta CHDs cannot be verified or hashed without the parent; the parent is located by its overall SHA-1. EmuWiz must model `ParentRequired` explicitly (section 6) and never attempt to flatten (section 11).

### 2.7 Writable vs read-only CHDs

**DOCUMENTED FACT** (chdman verify): *"The input file must be a read-only CHD format file (the integrity of writable CHD files cannot be verified)."* Writable CHDs (created with `-c none`) store hunks in a different map form and cannot be integrity-verified. **CONCLUSION FROM SOURCE**: a writable CHD is a legitimate, named `UnsupportedChd`/`NeedsReview` outcome, not a silent skip.

---

## 3. DAT ecosystems — how discs are represented

### 3.1 Redump

**INFERENCE + CONCLUSION FROM SOURCE (Redump DAT structure, confirmed against `redump_disk_records` test, logiqx.rs:1110-1136, and live Redump DAT conventions)**: Redump publishes **one `<rom>` per track** of the original BIN/CUE dump — e.g. `Game (Track 1).bin`, `Game (Track 2).bin`, plus a `.cue` — each with `crc32`/`md5`/`sha1` of that track's **BIN file bytes**. There is **no CHD hash, no CHD SHA-1, no logical-SHA1 field, and no metadata-derived identity** in a Redump DAT.

**Conclusion**: Redump DATs store *per-track BIN hashes* of the *original disc dump*. A CHD is not representable in a Redump DAT except as a filename-only entry. Verifying a CHD "against Redump" therefore requires **reconstructing per-track BIN byte streams from the CHD and hashing those** — which is possible *in principle* (section 7) but is a P1/P2 problem, not a direct-hash P0.

### 3.2 MAME Software Lists

**DOCUMENTED FACT** (live `hash/psx.xml`, `hash/dc.xml` at master): CD/DVD/GD-ROM software list entries are CHDs referenced as `<disk name="..." sha1="..."/>`, where `sha1` is the **CHD's overall SHA-1**. Example (psx.xml:50): `<disk name="007 Racing (USA)" sha1="c0fffd6939c403a0b7a179b472ae768c06c05c26"/>`. **CONCLUSION FROM SOURCE**: MAME software lists are the one ecosystem where a CHD's header overall SHA-1 is directly comparable — a **zero-decompression, read-header-only** exact match. This is the natural P0 CHD DAT target. Caveat: EmuWiz's Logiqx parser currently ignores `<disk>` elements, so a parser extension is required (section 15).

### 3.3 TOSEC / No-Intro / generic Logiqx-ClrMamePro

- **TOSEC**: cartridge/floppy-oriented, per-file hashes; no CHD convention of note. **UNCERTAIN** — no authoritative TOSEC CHD publication found in this pass.
- **No-Intro**: cartridge-first; disc systems are generally outside its core focus; where disc sets exist they are not CHD-hash oriented. **UNCERTAIN** but low relevance.
- **Generic Logiqx/ClrMamePro**: a DAT author *could* put a CHD's overall SHA-1 into a `<rom sha1=...>` field or a `<disk sha1=...>` element; there is no universal convention. **Do not invent a universal mapping where none exists**: the safe rule is — a DAT entry is CHD-comparable **only if its SHA-1 equals a CHD's header overall SHA-1** (i.e. the DAT author published CHD identities, as MAME does), and that is a *runtime equality test*, not an assumption.

### 3.4 Summary table

| Ecosystem | Stores | CHD-comparable? | How |
|---|---|---|---|
| Redump | per-track BIN crc/md5/sha1 | No (direct) | Only via reconstruction (§7) |
| MAME software lists | CHD overall SHA-1 in `<disk>` | **Yes (direct)** | Read header `sha1`, compare — P0 |
| TOSEC | per-file hashes | No | n/a |
| No-Intro | cartridge hashes | No | n/a |
| generic Logiqx/CMPro | whatever author chose | Only if equals overall SHA-1 | runtime equality test |

---

## 4. chdman (MAME) — capabilities for read-only verification

### 4.1 Licence

**DOCUMENTED FACT** (MAME `docs/source/license.rst`): the MAME project as a whole is GPL-2.0-or-later, but *"A great majority of files (over 90% including core files) are under the 3-Clause BSD License"* and contributors are encouraged to use BSD-3-Clause. **Verified at source level**: `src/tools/chdman.cpp`, `src/lib/util/chd.cpp`, `chdcodec.cpp`, `cdrom.cpp`, `hashing.cpp`, `flac.cpp` each carry `// license:BSD-3-Clause` SPDX headers. "MAME" is a registered trademark requiring permission for use in a name/logo/wordmark. **CONCLUSION**: distributing/embedding chdman or calling it from EmuWiz raises no GPL copyleft for the CHD tool itself (it is BSD-3-Clause); only the MAME name/wordmark is restricted. `chd-rs` is BSD-3-Clause and `libchdman-rs` is BSD-3-Clause (wrapping those BSD-3-Clause MAME files) — no licensing blocker either way (§17.I).

### 4.2 CLI stability & machine-readability

- CLI shape `chdman <command> <option>...` has been stable for over a decade; command names (`info`, `verify`, `createcd`, `extractcd`, ...) are stable.
- **No JSON / structured output.** `info`, `verify`, `dumpmeta` print human text to stdout; progress goes to stderr with `\r` overwrites. **CONCLUSION FROM SOURCE**: output is not a stable machine contract — parsing is fragile and must be treated as display text, not API. There is no `--json` flag in chdman.rst.
- `info` (chdman.cpp `do_info`): prints version, logical size, hunk size, total hunks, unit size, total units, compression list, CHD size, ratio, `SHA1:` (overall), `Data SHA1:` (raw; v4+), `Parent SHA1:`, and the full metadata list. **Read-only, cheap** (header + metadata read; no decompression).
- `dumpmeta` — prints a specific metadata tag's contents to stdout (or `-o file`). Read-only, cheap.
- `verify` (chdman.cpp `do_verify`): recomputes **raw SHA-1 by decompressing the entire logical data** in buffered chunks, compares with the header `rawsha1`; for v4+ also computes the **overall SHA-1** from raw+metadata and compares with header `sha1`. `--fix` rewrites the header (mutating — **never allowed in EmuWiz verification**). Success prints `Raw SHA1 verification successful!` / `Overall SHA1 verification successful!`; mismatch calls `report_error(1, ...)` which throws `fatal_error` → nonzero exit. Uncompressed CHD → `report_error(0, "No verification to be done; CHD is uncompressed")`. **CONCLUSION FROM SOURCE**: `verify` is a *full-decompression* pass (same cost as extraction), exit code is meaningful (0 success, 1 failure), but the only way to know *why* is stderr text.
- `extractcd`/`extractdvd`/`extracthd`/`extractraw`: write **files to disk** (CUE+BIN, optionally `--splitbin`, or ISO/raw). **Output is a file path; there is no stdout streaming of content.** `extractcd` requires knowing the output layout up front and refuses existing files without `--force`.
- Subprocess argument injection: chdman takes file paths as argv; with EmuWiz's argv-array `Command::new(...).args(...)` pattern (no shell), injection is not a vector (section 10).
- Version compatibility: old chdman cannot read new codecs (zstd/cdzs); new chdman reads old CHDs. `info` refuses v<3. **CONCLUSION**: any chdman dependency must pin a minimum MAME version and treat "unknown compression" as a named failure.

### 4.3 Fit summary

`info`/`dumpmeta` = cheap, read-only, but text-only. `verify` = correct but as expensive as extraction and text-only. `extract*` = writes files (temp-space, mutation-adjacent), never streams. No JSON. chdman is a *correct reference oracle* and a *poor in-process verifier*.

---

## 5. Native Rust vs chdman vs optional helper

### 5.1 Option A — native Rust CHD reader

Two viable crates:

**`chd` / `chd-rs` (SnowflakePowered)** — pure safe Rust, BSD-3-Clause, MSRV 1.59. Drop-in for libchdr, verified against `chd.cpp`. Supports v1-5 (v5 fully; v1-4 "not as rigorously tested"), all v5 codecs including `cdlz`/`cdfl`/`cdzl`/`cdzs`/`zstd`/`lzma`/`huff`/`flac`/`avhu`. Streaming: `Chd::open(&mut f, parent)` (parent = `Option<Box<Chd>>`), iterate hunks `0..hunk_count()`, `read_hunk_in`, plus buffered `Read+Seek` adapters (`ChdReader`) and metadata iteration (`metadata_refs`). Header exposes `rawsha1`/`sha1`/`parent_sha1`. Optional `verify_block_crc` feature verifies per-hunk CRC-16. `rchdman` (same repo) already implements read-only `info`, `verify` ("Metadata integrity is not verified"), `extractraw`, `dumpmeta`. No write operations and none planned. Performance within ~15% of libchdr (pure-Rust codecs), ~1% with `max_perf` (zlib-ng + lzma-rs fork).

**`libchdman-rs` (danifunker)** — Rust wrapper that **compiles MAME's actual C++ `chd.cpp`** (via `cc`), BSD-3-Clause, version == embedded MAME version (0.289.0). "100% feature parity with chdman." Adds `CdCookedReader` (cooked 2048-B/sector `Read+Seek` ISO stream from a CD CHD — skips sync/header/ECC/subcode) and `cd::list_tracks`. Ships prebuilt static archives for many targets (download-on-build) or requires a C++20 toolchain + ~900 MB MAME source for source builds.

### 5.2 Option B — shell out to chdman

Pros: bit-perfect correctness by construction, zero Rust codec maintenance, audit-trail of a well-known tool. Cons: requires MAME installed (config key like `ratarmount_bin`, config.toml.example:38 pattern); text-only output parsing; `verify`/`extract` write temp files or have 64 KiB/30 s runner limits; no streaming to stdout; exit-code-only signals; portability (user must install on each OS); process/network surface contrary to the codebase's identity-read lint (game_identity.rs:2693-2716).

### 5.3 Option C — optional external helper (bundle or recommend chdman, native-first)

Native Rust reader as the primary path; chdman as an *optional, user-installed oracle* for cross-checking `verify`/`info` when present. Keeps the default path self-contained and sandboxable, adds a belt-and-suspenders oracle for users who want it.

### 5.4 Comparison

| Axis | A: chd-rs (native) | A: libchdman-rs | B: chdman subprocess | C: native + optional oracle |
|---|---|---|---|---|
| Complexity | low-med (pure Rust dep) | med (C++ FFI build/trust) | low code, high ops | med |
| Licence | BSD-3 | BSD-3 | BSD-3 (tool) | BSD-3 |
| Maintenance | upstream crate; small team | upstream crate; 1-fork project | MAME upstream (large, stable) | both |
| Correctness | verified vs chd.cpp; codecs v5 | parity by construction | reference | reference + native |
| CHD versions | v1-5 (v5 full) | v1-5 (MAME core) | v1-5 (MAME core) | same |
| Streaming | yes (hunk `Read`+`Seek`) | yes (CdCookedReader) | no content stdout | yes |
| Sandboxing | in-process, no exec | in-process | external process | in-process (+optional exec) |
| Portability | pure Rust | prebuilt matrix / C++ toolchain | user-installed binary | pure Rust |
| Performance | ~85-99% of libchdr | == MAME | process spawn + full-decompress | native fast-path |
| Testability | unit-testable, fixtures | unit-testable | integration-only | both |

**Recommendation**: **Option A with `chd-rs` (pure Rust) for P0**, with **Option C's oracle (optional chdman) deferred to P1+** as a cross-check. Rationale: P0 needs *read-only header identity + bounded streaming integrity*, both of which `chd-rs` does in-process with no exec, no temp files, full cancellation/progress control, and clean licensing — matching the codebase's strong read-only/sandbox discipline (safe_read, identity-read lint, no-network). `libchdman-rs` is powerful (CdCookedReader is genuinely useful for P1 ISO-level reads) but pulls in a C++ compile/prebuilt-download supply chain that is disproportionate for a P0 header+integrity pass.

---

## 6. Verification modes (result states)

Only recommend states that are justified by evidence (§2-5). Map onto the archive-aware research's two-axis model: `AuditVerdict` (hash-strength) stays untouched; a `MatchProvenance`-axis carries the CHD source/transform; CHD refusals are **pre-verdict refusal records**, exactly where `HashRefusal` already sits for loose files (audit_run.rs:421-434), and `NeedsReview` is a GUI rollup of specific reasons (ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md §8).

| State | Meaning | Justification |
|---|---|---|
| `ExactChdContainer` | Header overall SHA-1 == DAT entry SHA-1 (MAME software lists / CHD-aware DAT) | Direct, zero-decompression; strongest CHD identity (§2.5, §3.2) |
| `ExactChdLogicalHash` | Streaming recompute of `rawsha1` (with per-hunk CRC-16 when enabled) matches header `rawsha1` | Integrity self-check; proves the logical stream is intact, not that it matches a DAT |
| `ExactAfterChdExtraction` | Reconstructed per-track BIN byte streams hash-match Redump-style DAT track entries (in-memory reconstruction, no temp files) | P1/P2; §7 — only when reconstruction is deterministic |
| `ChdMetadataMatch` | TOC/track structure (CHT2/CHT2 tags, disc type, track count/types) matches DAT or is internally consistent | Weak-to-moderate evidence; **must never be promoted to Exact** — metadata is derived, not cryptographic identity |
| `ParentRequired` | `parentsha1 != 0` and parent not located | Hard format fact (§2.6) |
| `UnsupportedChd` | Unsupported version/codec (e.g. unknown compression, writable CHD, zstd on old reader) | Named refusal, per §2.7/§4.2 |
| `CorruptChd` | Header magic/version invalid, map malformed, hunk CRC-16 mismatch, decompression error, size inconsistency | Named refusal, never partial hash (mirrors `InspectorError::Malformed`, inspector.rs:224-227) |
| `AmbiguousChdRepresentation` | Same logical disc reachable via multiple distinct DAT identities, or a CHD whose reconstruction could match >1 track set | Fail-closed, mirroring `AmbiguousNormalization` (§6 of archive-aware research) |
| `NeedsReview` | GUI rollup for any of the refusal/ambiguous states | Presentation-only (§8 of archive-aware research) |
| `NotInCatalogue` | Successfully verified raw+overall integrity but matches no DAT entry | Ordinary `NotInDat` reached via CHD provenance |

Rules: `Exact*` states require a **byte-exact cryptographic hash match** on a defined byte stream (header SHA-1 for container; recomputed raw for integrity; reconstructed tracks for extraction). No fuzzy/partial credit. `ChdMetadataMatch` is never `Exact`. `ParentRequired`/`UnsupportedChd`/`CorruptChd` are pre-verdict refusals (like `HashRefusal`), never `AuditVerdict`s. `AmbiguousChdRepresentation` is never guessed.

---

## 7. Redump / CD case (high priority)

### 7.1 The core question

Can a CHD created from a Redump-style BIN/CUE be verified back against Redump per-track hashes **without permanently extracting it**?

### 7.2 What the CHD stores vs. what Redump hashes

**CONCLUSION FROM SOURCE (chd.cpp/cdrom.cpp/chdman.cpp) + INFERENCE**:
- Redump hashes each `.bin` file: `frames × datasize` bytes (typically 2352-B raw sectors for data tracks, 2352-B audio sectors for audio), no subcode in the `.bin`, no 4-frame padding, pregaps expressed in the `.cue`, subchannels in optional sidecars (`.sub`/`.sbi`).
- A CHD stores the **same sectors but re-framed**: 2448 B/frame (2352 + 96 subcode), **padded to 4-frame boundaries per track**, audio possibly FLAC-compressed, subcode stored inline, pregap/postgap as metadata fields (`CHT2`). `extractcd` reverses this on the read side.
- The CHD **does not store** the Redump track BIN hashes, the `.cue` text, or the subchannel sidecar. It stores the sector data + the structural metadata needed to re-derive them.

### 7.3 Determinism of reconstruction

**INFERENCE, grounded in the format**:
- **Yes, deterministic in the common case**: for standard discs (mode-1 data track + red-book audio), `extractcd` produces per-track BIN byte streams that, modulo subcode and padding, equal the source Redump BIN bytes. Redump's own community tooling and multiple emulators use chdman extraction as a valid BIN/CUE round-trip for exactly this reason.
- **Not guaranteed universally**: (a) sector **type/submode** variants (Mode 2 Form 1/2, XA, mixed) require the CHD to have stored raw 2352-B sectors with correct type metadata; if the CHD was created from a *cooked* (2048-B) source, `MODE1`-promotion re-synthesizes sync/header/mode bytes that are **not** the original ECC/EDC (cdrom.cpp:499-521 explicitly logs "promotion of mode1/form1 sector to mode1 raw is not complete!"); (b) **pregap handling** — Redump hashes sometimes include or exclude pregap frames depending on rip style; (c) **subchannels** — Redump `.bin` hashes exclude the 96 B subcode, so the verifier must strip it; (d) **write offsets / session layout** in a CHD are metadata, not bytes, and can disagree with the original.
- **Conclusion**: track reconstruction is **deterministic in principle but not universally byte-exact** against every Redump rip. Verification must **fail closed** (never "close enough"), which means the P1/P2 path can only claim `ExactAfterChdExtraction` when the reconstructed stream is byte-identical to a DAT track hash — and where chdman's documented sector-promotion caveat applies, the result is `NeedsReview`/`CorruptChd`-class, not a downgraded match.

### 7.4 Without extraction at all

- chdman exposes the **overall SHA-1** and **raw SHA-1** directly (`info`/header), but neither equals a Redump track hash (§2.5, §7.2). **Conclusion**: chdman's exposed hashes are insufficient for Redump comparison.
- **In-memory reconstruction** (decompress hunks → strip 96-B subcode → trim 4-frame padding → split by `CHT2` track boundaries → hash per track) avoids permanent extraction and is the *only* "no permanent extraction" path that can hit Redump. This is P1/P2 work and needs the sector-type discipline of §7.3. **INFERENCE**: for single-track mode-1 data discs (e.g. many PS1/PC-Engine/3DO titles) this is tractable and deterministic; for multi-track audio/mixed discs it is where the caveats concentrate.
- When verification is impossible without extraction/reconstruction: any disc whose CHD sector type metadata does not positively match the DAT's implied track layout (unknown type, cooked-only source, session/pregap ambiguity) must be `NeedsReview`, **not** approximated.

---

## 8. DVD / GD-ROM / other disc types

| Type | CHD variant (metadata tag) | Practical? | Platform-specific rules |
|---|---|---|---|
| PS2/DVD | `createdvd`, `DVD ` | Yes — DVD CHDs are raw unit streams, no CD framing; raw SHA-1 == plain ISO-stream SHA-1, so a DAT carrying the DVD ISO SHA-1 is directly comparable | Use `createdvd` (not `createcd`) for DVD titles; `createcd` for PS2 CD titles (documented PS2 issue, gametechwiki). |
| GameCube/Wii (mini-DVD) | `createdvd` on GCM/ISO | Yes — effectively an ISO stream; the existing `inspect_rvz`/`inspect_ciso`/`inspect_wbfs` header readers (game_identity.rs) show the codebase already reads this family's headers | EmuWiz platform registry already treats `iso/gcm/gcz/rvz/ciso` for GC/Wii (database.rs:3995-4010); add `chd`. |
| Dreamcast GD-ROM | `createcd` + `CHGT`/`CHGD` (high-density area) | Yes but **GD-ROM specific**: chdman auto-`--splitbin` for GD-ROM CUE output; GD-ROM track metadata adds `PAD` | GD-ROM extraction differs from Redump CD; treat as its own case. |
| Sega CD / Saturn / PC Engine CD / 3DO / PS1 | `createcd` (CD) | Yes; all standard-CD CHDs | Multi-track audio discs → §7 caveats. |
| Arcade CHDs | MAME ROM sets (`<disk>`/CHD), often parent/child | Yes — MAME publishes overall SHA-1; `parentsha1` common | Arcade `.chd` files "whose names are the only identification" (platform/mod.rs:405) — name + MAME DAT, not content-based. |
| Hard disks (createhd) | `GDDD` geometry | Partial — only container/overall-SHA-1 or full raw-stream hashing; no disc-track DAT generally | EmuWiz does not catalogue hard disks today; defer. |
| LaserDisc | `createld`/`AVAV` | Defer — A/V Huffman codec, frame-based | Out of scope for P0/P1. |

**INFERENCE**: CHD is practical for every optical platform EmuWiz already lists as a `chd` weak extension (platform/mod.rs), but the *verification strategy* splits cleanly: **(1) DVD/HD-type CHDs** (raw unit streams) can support direct logical-hash comparison if a DAT carries the ISO/raw SHA-1; **(2) CD-type CHDs** need track reconstruction (§7); **(3) GD-ROM** needs GD-specific handling; **(4) arcade** is name+MAME-DAT; **(5) LD/HD** deferred. Platform-specific rules are unavoidable for the CD family's sector/pregap/subcode semantics — that is why reconstruction is a separate phase, not part of P0.

---

## 9. Performance / safeguards

- **Bounded concurrency**: keep the archive-aware research's sequential-per-outer-file model (audit_run.rs is sequential; no thread/rayon). CHD per-hunk decompression may use the crate's internal codecs but EmuWiz should not add unbounded parallelism in P0.
- **Cancellation**: thread the existing `&AtomicBool` into the hunk loop; check once per hunk and inside the chunked hashing loop, mirroring `hash_file_reporting` (checked per chunk, hashing.rs:19-22, 288-330). For a multi-GB CHD this is mandatory, not optional.
- **Progress**: `DatAuditProgress::Hashing{index,total,file_name}` (audit_run.rs:130-135) reports per outer file. For CHD, report **per-hunk or per-percentage-of-logicalbytes**, not per byte (same rationale as the archive-aware research: per-chunk over thousands of files is noise).
- **Streaming**: P0 integrity = decompress hunks sequentially into a bounded hunk buffer and feed a hasher; never materialize the logical image. `chd-rs`'s `ChdReader`/`read_hunk_in` give exactly this.
- **No `/tmp` explosions**: P0 has **no temp file at all** — verification is in-memory streaming (this must be an explicit invariant + test, mirroring `inspection_performs_no_filesystem_writes`, inspector.rs:712-747). Extraction/reconstruction (P1+) must estimate temp space from `logicalbytes` and refuse if free space < logicalbytes + margin before writing a byte (the "temp-space estimation" guard).
- **Cache strategy**: §13. Reuse of header SHA-1s is the big win: reading the header is ~124 bytes + map/metadata; a container `ExactChdContainer` match against a MAME-style DAT needs **no decompression at all**.
- **Avoid repeated decompression**: cache raw-SHA-1 verification results keyed by container identity (§13); re-verifying a CHD whose header SHA-1s and fingerprint are unchanged is wasted work.

---

## 10. Security / safety

Verification must remain read-only. Specifics:

- **Malformed CHDs**: validate header magic (`'MComprHD'`), version, `length`, `hunkbytes ≤ 1 MiB`, `unitbytes > 0`, `logicalbytes` consistency with hunk count, map/meta offsets within file bounds, hunk count not absurd. All map reads bounded. Any inconsistency → `CorruptChd`, never a partial hash.
- **Decompression bombs**: the map's declared `logicalbytes` is the decompressed upper bound; a CHD is *not* arbitrary-ratio like a ZIP member — per-hunk output is fixed at `hunkbytes`. Guard against (a) `logicalbytes` far exceeding physical file size (bomb), (b) a hunk whose compressed length exceeds `hunkbytes` without ratio checks, (c) unbounded metadata blobs (bound metadata read to a small cap, e.g. 1 MiB, and total metadata count). Enable per-hunk CRC-16 verification where the reader supports it (`verify_block_crc`).
- **Parent references**: `parentsha1 != 0` ⇒ `ParentRequired`; never try to open a guessed parent path. If a parent is supplied/located, it must itself be opened through `safe_read` and validated before any PARENT-map entry is read.
- **Path handling**: outer `.chd` opened only through `safe_read`/`open_bounded_read` (TrustedRoots, O_NOFOLLOW, dev/inode re-check). Metadata contents are data, never paths; do not turn metadata strings into filesystem operations.
- **Subprocess argument injection (if chdman ever used)**: use `Command::new(program).args([...])` (argv array, no shell) exactly as `run_command_os_with_timeout` does; never interpolate into a shell string. Treat all file paths as opaque argv elements.
- **Hostile metadata**: metadata is checksummed (part of overall SHA-1) and must be read bounded + validated (tags known set, sizes sane). Displayed raw, never interpreted as code/HTML/paths.
- **Symlinks**: covered by safe_read for the outer file; CHD internal references are hunks/metadata, never filesystem links.
- **Huge outputs**: P0 never writes output; P1+ reconstruction writes only after temp-space check and with a hard cap; progress + cancellation mandatory.
- **Partial/corrupt images**: a mid-stream decompression error → `CorruptChd` with the hunk index in the refusal detail; no partial verdict.

**Source-mutation guarantee**: the CHD open path must use read-only semantics only (no `chd` write APIs, no `--fix`, no `addmeta`/`delmeta`, no re-compress, no flatten). A test mirroring `inspection_performs_no_filesystem_writes` (inspector.rs:712-747) must assert byte-for-byte CHD equality before/after a verify pass, including mtime.

---

## 11. Source mutation — explicit never-list

Verification must **NEVER**:
- convert a CHD to any other format (BIN/CUE, ISO, raw);
- recompress a CHD (different codecs/hunk size);
- rewrite CHD metadata (`addmeta`/`delmeta`, including `-nocs`);
- flatten a parent/child relationship into a standalone CHD;
- extract permanently to a user-visible location;
- run `chdman verify --fix`;
- alter source files in any way (no writes, no mtime changes).

Any conversion/extraction feature (e.g. a user-initiated "extract this CHD to BIN/CUE" action) must be a **separate, explicit, later feature** with its own UI and its own "this writes files" disclosure — never a side effect of verification. This mirrors the archive-aware research's invariant (renaming/rewriting are never part of verification).

---

## 12. Rename / organisation

**INFERENCE + CONCLUSION FROM SOURCE (rename_plan/model.rs:171-219)**: if a CHD verifies against a DAT entry representing BIN/CUE or ISO:
- **Rename the outer `.chd` only** — `RenameProposal.source_path` (model.rs:175) continues to mean "the path on disk to rename", which for a CHD match is the `.chd`'s own path, exactly as for a ZIP archive match (archive-aware research §9).
- **Preserve inner/logical provenance** — the proposal must carry the CHD provenance (container SHA-1, raw SHA-1, disc type, track count, the DAT entry's intended BIN/CUE/ISO identity) as additive fields, so the user sees "this .chd is verified as Redump track-set X" without the filesystem suggesting a conversion.
- **Never silently convert back to BIN/CUE** — a verified CHD is *not* "missing its BIN/CUE"; the rename engine must not propose extraction. `ExtensionStatus` (model.rs:104-112) must evaluate the archive's own `.chd` extension (`Preserved`), never imply a `.bin`/`.cue` result.
- The existing `ProposalState::Unsupported` doc already names "the DAT entry names an internal archive member whose extension differs" (model.rs:69-71) as an unsupported case; CHD-as-container should be the *supported* generalization of that (the DAT names a track set, the file is a container — rename the container).

---

## 13. Cache model

Propose keys using the archive-aware research's cache precedents (`FileFingerprint` path+size+mtime; `StableFileMetadata` +dev+inode; `IdentityCache`/`CACHE_FORMAT_VERSION` atomically-swapped JSON; `CLASSIFIER_VERSION`).

Safe cache key for a CHD verification result:
- **Outer file identity**: path + size + mtime (+ Unix dev/inode) — reuse `FileFingerprint` shape (never path alone).
- **Container identity**: header `sha1` (overall) + `rawsha1` + `parentsha1` + `version` + `compressors[0..4]` + `hunkbytes` + `logicalbytes` + `unitbytes`. Any of these differing invalidates the cached verdict. This is the "reuse CHD internal SHA1s where trustworthy" mechanism: the overall SHA-1 is itself a content+metadata digest, so it is a *strong* cache key — but always cross-checked against `FileFingerprint` because a header could in principle be stale.
- **Logical identity** (for `ExactChdLogicalHash`): the recomputed raw SHA-1.
- **DAT-source dimension**: key by which DAT source(s)/version produced the match, so updating the DAT invalidates cached `NotInCatalogue`/`AmbiguousChdRepresentation` results (the archive-aware research requires exactly this for ambiguous results).
- **Verifier version**: a `CHD_VERIFIER_VERSION` constant, bumped whenever CHD verification *rules* change (new codec handling, new sector-type logic, changed reconstruction rules) — modeled on `CLASSIFIER_VERSION` (classification.rs:16) and `NORMALIZER_VERSION` (archive-aware research §10), so a cached verdict is never silently reinterpreted under new rules.
- **What must never be cached as final**: `CorruptChd`, `ParentRequired` (parent state can change), and `AmbiguousChdRepresentation` (DAT-set-dependent). These are either not cached or invalidated by any of the key components changing.

---

## 14. Test vectors

Prefer public/legal or tiny synthetic fixtures. Note licensing: game-derived CHDs (arcade, TombRaider parent example used by chd-rs) are copyrighted data — do not commit those; generate synthetic fixtures instead.

| Case | Vector | How to obtain |
|---|---|---|
| Valid CHD v5 | tiny `createhd` blank or `createraw` fixture | `chdman createhd -o t.chd -s <small>` or generate with chd-rs in tests; commit a tiny (KBs) synthetic CHD |
| Corrupt CHD | valid fixture with flipped header/map bytes | byte-mutate in test (read-only fixture + corrupt a copy in temp) |
| Parent CHD | two `createhd` CHDs, second `--outputparent` delta | chdman or chd-rs; synthetic (blank + delta) |
| CD CHD | `createcd` from a tiny hand-made CUE+small BIN | build minimal CUE with one mode-1 track of N sectors + audio sector; chdman/createcd |
| DVD CHD | `createdvd` from a tiny ISO (e.g. 4 KiB ISO9660 image) | chdman/createdvd on a synthetic ISO |
| Single-track CD | one data track, no audio | above |
| Multi-track audio/data CD | data track + 1-2 audio tracks | CUE with `MODE1/2352` + `AUDIO` tracks; chdman |
| CHD matching known Redump source | a Redump-track DAT whose track hashes equal the reconstructed streams of the above synthetic disc | construct DAT with computed track hashes from the *original* BIN bytes; assert `ExactAfterChdExtraction` when deterministic, and document the sector-type conditions |
| CHD that cannot be reconstructed unambiguously | cooked-2048 source CHD, or a CHD whose sector type metadata is ambiguous | `chdman createcd` from a 2048-B "cooked" source → assert `NeedsReview`/refusal, never a downgraded match |
| Cancellation during verification | multi-hunk synthetic CHD + cancel flag set mid-loop | integration test with `&AtomicBool` |

Every CHD integration test should also assert **no filesystem writes** (mirror `inspection_performs_no_filesystem_writes`, inspector.rs:712-747) and no mtime change on the fixture.

---

## 15. Integration with the existing archive-aware design

Do **not** create a parallel verification engine. The archive-aware research defines the general pipeline; CHD is a new **container/type adapter** inside it.

Preferred shape:

```
FileCandidate
  └─ container/type adapter         (audit_run.rs per-file loop: "is this a CHD?")
       └─ CHD adapter               (new: CHD open via chd-rs through safe_read)
            ├─ header identity      → overall/raw/parent SHA-1, version, codecs, geometry
            ├─ metadata             → CHT2/CHGD/DVD/GDDD tags, track TOC, disc type
            └─ logical stream       → streaming hunk reads (raw verification)
                 ├─ existing hash/index lookup   (DatIndex: crc/md5/sha1/sha256; audit_one)
                 ├─ normalization if applicable  (P1+: track reconstruction = CHD "normalization")
                 └─ provenance result            (MatchProvenance: ChdContainer/ChdLogical/ChdTrack…)
```

Concrete mapping onto existing code:
- **Detection**: extend `archive_kind` (`lib.rs:3296`) to map `.chd` to a new kind (e.g. `ArchiveKind::Chd`), so `.chd` becomes a catalogued/watched format; add `chd` to `watch_path_is_supported_archive` (lib.rs:5556) and `source_assignment_is_compatible` (database.rs:3995-4010) for the platforms that use it.
- **Identity**: replace the `Deferred` arm at game_identity.rs:513 with a header-only CHD reader (title/region hints from metadata where present) — but keep `Deferred` semantics if platform trust is absent. Keep the "no existing safe bounded reader" honesty until a bounded reader exists.
- **DAT audit**: in `run_dat_audit`'s per-file loop (audit_run.rs:396-436), branch on CHD detection: instead of opaque whole-file hashing, produce `KnownFileEvidence` from the CHD adapter (header overall/raw SHA-1, plus reconstructed track hashes in P1+), and feed `audit_one`/`DatIndex` as today. **CONCLUSION FROM SOURCE**: this keeps a single comparison path; the CHD adapter is the only new surface.
- **DAT parsing**: extend `logiqx.rs` to ingest MAME software-list `<disk name=... sha1=.../>` elements as CHD-comparable entries (P0 need for `ExactChdContainer`); leave Redump track entries as plain `<rom>`s.
- **Provenance**: add `MatchProvenance` variants (ChdContainer / ChdLogicalHash / ChdTrack) and the refusal records from §6, exactly where the archive-aware research puts `ArchiveMember`/`Normalized*` (§8 of that doc). The GUI (`dat_sources_page.rs` verdict rows) needs new category rows only for the states in §6 that are reachable.
- **No second scanner**: the walker (`scan_local_files`), bounds, cancellation, progress, and comparison engine are all reused; only the per-file adapter is new.

---

## 16. P0 / P1 / P2 plan

**P0 — CHD detection + read-only identity + integrity (no extraction):**
- Detect `.chd` (archive_kind, watch list, source-assignment).
- Add `chd`/`chd-rs` dependency; open through `safe_read`.
- Read header: overall/raw/parent SHA-1, version, codecs, hunk/unit/logical bytes, metadata tags (CHT2/CHGD/DVD/GDDD) → CHD provenance.
- `ExactChdContainer` against MAME software-list `<disk>` DATs (needs Logiqx `<disk>` parsing).
- Streaming `ExactChdLogicalHash` integrity pass with cancellation, progress, per-hunk CRC-16, bounds, no temp files, no writes.
- Refusal states: `CorruptChd`, `UnsupportedChd`, `ParentRequired`, writable-CHD handling.
- Cache with the §13 key; no-filesystem-writes tests; synthetic fixtures (§14).
- GUI: add the §6 verdict rows/provenance display.

**P1 — CD/DVD logical verification where exact mapping is proven:**
- DVD/HD raw-unit-stream direct hashing vs DATs that carry the ISO/raw SHA-1 (PS2 DVD, GC/Wii via `createdvd`).
- In-memory CD track reconstruction for the deterministic subset (§7.3): strip subcode, trim padding, split by CHT2, hash per track → `ExactAfterChdExtraction` against Redump-style DATs, **only** where byte-exactness is provable; otherwise `NeedsReview`.
- Optional chdman oracle cross-check (Option C), user-installed via a `chdman_bin` config key, using the argv-array runner with widened limits.

**P2 — broader support:**
- GD-ROM specifics (high-density area, GD-ROM metadata, split-bin semantics).
- Parent/child CHD resolution (locate parent by overall SHA-1, verify deltas).
- Multi-track/mixed-mode reconstruction hardening; sector-type ambiguity → fail-closed.
- Remaining platforms (Saturn/PS1 audio-heavy, PCE, 3DO, Sega CD); arcade CHD name+DAT matching; hard-disk/LaserDisc if ever catalogued.

Derivation from evidence: P0 = header identity + integrity (what the format and MAME DATs make *directly* and *safely* comparable today); P1 = the proven-deterministic reconstruction subset; P2 = everything whose byte-exactness is not yet established (§7.3/§8). Extraction/reconstruction is never part of P0.

---

## 17. Bottom-line answers

**A. Is CHD-aware verification safe to build now?**
Yes, for the read-only envelope: container detection, header identity, and streaming integrity verification are safe to build now using a pure-Rust in-process reader, because every primitive (bounded open, chunked cancellable hashing, collision-aware DAT index, pre-verdict refusal records) already exists in EmuWiz and the CHD format's own hashes are read-only header fields. What is **not** safe to build now as a "match" is any claim that a CHD's bytes equal a Redump BIN/CUE hash without reconstruction — that must stay a separately-phased, fail-closed path.

**B. Should P0 use chdman or native Rust?**
Native Rust (`chd-rs`) for P0: read-only header identity + streaming integrity in-process, no exec, no temp files, clean BSD-3-Clause licence, full cancellation/progress control. chdman is a great *reference oracle* and correct for `verify`, but its text-only output, no-content-stdout, temp-file extraction, and external-install requirements make it a poor P0 engine; defer it to P1 as an optional cross-check (Option C).

**C. Can CHDs be verified against ordinary Redump DATs without extraction?**
No. Redump DATs store per-track BIN hashes; a CHD stores a padded 2448-B/frame stream plus structural metadata, and its own raw/overall SHA-1s are not equal to any Redump track hash (§2.5, §7.2). Without extraction, only **in-memory reconstruction** (P1+) can reach Redump hashes — and only for the deterministic subset (§7.3); otherwise the honest result is `NeedsReview`, never a near-match. Direct (zero-decompression) CHD-vs-DAT matching works today only against DATs that publish the CHD overall SHA-1 (MAME software lists).

**D. Which CHD media types are safe for P0?**
Optical disc CHDs whose verification is header-identity + integrity only (any CD/DVD/GD/HD type can be *detected, proven intact, and provenance-tagged* in P0 — safety comes from not claiming a DAT match where none is provable). For *DAT-matching specifically*: DVD/raw-unit-stream CHDs against ISO/raw-SHA-1 DATs, and any CHD against MAME-software-list `<disk>` SHA-1s. CD-type DAT matching is P1+ (§8).

**E. When must results stay NeedsReview/Ambiguous?**
Whenever byte-exactness cannot be proven: cooked-source sector promotion (cdrom.cpp "not complete!" caveat), ambiguous sector type/submode, pregap/session/subchannel uncertainty vs. a given Redump rip, multiple distinct DAT identities reachable from one logical disc, missing parents, unknown codecs/versions, and any writable/unverifiable CHD. `AmbiguousChdRepresentation`/`NeedsReview` is always safer than a downgraded match.

**F. Should CHD conversion ever be part of verification?**
No. Verification is read-only by definition. Conversion/extraction is a separate, explicit, user-initiated feature with its own disclosure and temp-space handling (§11). Verification must never convert, recompress, rewrite metadata, flatten parents, extract, or run `--fix`.

**G. What temp-space safeguards are mandatory?**
P0: **zero temp files** — in-memory streaming only, proven by a no-filesystem-writes test. P1+: if reconstruction ever writes, estimate temp space from `logicalbytes` and refuse before writing if free space < `logicalbytes` + margin; cap total output; progress + cancellation mandatory; clean up on any failure.

**H. Recommended first implementation slice?**
The CHD header/identity adapter in isolation: `archive_kind` `.chd` detection + `chd-rs` open through `safe_read` + header (overall/raw/parent SHA-1, codecs, geometry, metadata tags) + provenance record + `ExactChdContainer` match against a synthetic MAME-style `<disk>` DAT fixture — with the no-filesystem-writes and bounded-read tests. It has zero dependency on reconstruction, is the smallest independently testable unit, and de-risks the container-identity contract before any hunk streaming or track logic is written. Second slice: streaming integrity (`ExactChdLogicalHash`) on synthetic CD/DVD/HD fixtures (§14).

**I. Any licensing blockers?**
No. chdman's CHD core files are BSD-3-Clause (verified in MAME source headers); `chd-rs` is BSD-3-Clause; `libchdman-rs` is BSD-3-Clause. Only the MAME *name/wordmark* is trademark-restricted — EmuWiz must not use "MAME" branding in its own name/logo. No GPL copyleft applies to embedding/calling these.

**J. Any platform-specific blockers?**
No hard blockers for the safe envelope. The real platform constraints are *verification-strategy* constraints, not legal/technical blockers: CD-family sector/pregap/subchannel semantics (§7.3) gate Redump matching; GD-ROM needs its own handling; arcade relies on name+MAME-DAT; hard-disk and LaserDisc are out of scope until catalogued. None of these block P0 (detection + header identity + integrity for all CHD types).

---

## Sources / citations index

**Repository (this clone, researched at `f7c450c`, re-verified at `7c8d6ea`)**
- `crates/archivefs-core/src/inspector.rs` (LIKELY_CONTENT_EXTENSIONS, classify_entry, is_inspectable, is_known_disc_companion, INSPECTOR_ENTRY_LIMIT, tests)
- `crates/archivefs-core/src/game_identity.rs` (dispatch 497-526 incl. `Deferred` at 513; IdentityStatus 121-130; identity-read lint 2693-2716)
- `crates/archivefs-core/src/platform/mod.rs` (SHARED_EXTENSIONS 146, weak_extensions, arcade note 405)
- `crates/archivefs-core/src/lib.rs` (archive_kind 3296-3327, watch_path_is_supported_archive 5556-5559, command_available 7203-7212, run_command_os_with_timeout 7225-7283)
- `crates/archivefs-core/src/database.rs` (source_assignment_is_compatible 3995-4010)
- `crates/archivefs-core/src/dat/sources/audit_run.rs`, `dat/audit.rs` (AuditVerdict, audit_one ladder), `dat/index.rs`, `dat/model.rs`, `dat/parsers/logiqx.rs` (redump_disk_records 1110-1136)
- `crates/archivefs-core/src/identity_source/hashing.rs` (LocalHashes, hash_file_reporting, MAX_AUTOMATIC_HASH_BYTES), `verification.rs`, `cache.rs`
- `crates/archivefs-core/src/dat/rename_plan/model.rs`, `dat/classification.rs` (CLASSIFIER_VERSION)
- `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md` (companion; pipeline, provenance model, cache/normalizer-version precedents)

**External (fetched live during this research pass)**
- MAME `docs/source/tools/chdman.rst` and `docs.mamedev.org` chdman page — commands, options, compression algorithms, licence page.
- MAME `src/lib/util/chd.h` — v1-5 header layouts, map formats, metadata tags, error enums.
- MAME `src/lib/util/chd.cpp` — compute_overall_sha1 (1709-1751), read_metadata, codecs.
- MAME `src/lib/util/cdrom.h` / `cdrom.cpp` — FRAME_SIZE (2448), MAX_SECTOR_DATA/MAX_SUBCODE_DATA, TRACK_PADDING=4, sector types, subcode, MODE1 promotion caveat, write_metadata formats.
- MAME `src/tools/chdman.cpp` — do_info, do_verify (raw + overall SHA-1, --fix), do_create_cd (padding), do_extract_cd (CUE/BIN/GDI, splitbin), report_error semantics.
- MAME `hash/psx.xml`, `hash/dc.xml` — `<disk name=... sha1=.../>` software-list convention.
- crates.io / GitHub: `chd` (chd-rs, SnowflakePowered) README + source (chdfile.rs, header.rs, metadata.rs, read.rs); `libchdman-rs` README (CdCookedReader, prebuilt archives, licence); `opticaldiscs-rs`.
- Emulation General Wiki — "Save disk space for ISOs" (CHD archive-quality notes: PS1 "probably no", PS2 DVD "probably yes", GC/Wii, PSP, Dreamcast; createdvd vs createcd guidance; revert-ability caveats).

All file:line citations for repository code were read directly from commit `f7c450c` in this clone and re-verified against current `origin/main` at `7c8d6ea`. External CHD/chdman facts are from the pinned MAME sources/version cited above; MAME versions evolve, so codec/`<disk>` conventions should be re-verified against the MAME release EmuWiz targets before implementation.
