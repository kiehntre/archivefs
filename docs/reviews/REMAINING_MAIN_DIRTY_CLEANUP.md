# Remaining Main Dirty Cleanup — 2026-08-05

Release-preparation cleanup audit of the 47 dirty entries remaining in the protected main worktree (`/home/davedap/archivefs`) after the DAT Sources GUI Stage 1 merge (PR #7, merge commit `a144ed0`). Every entry is classified, verified byte-for-byte against its backup, and either removed from `main` (categories A and B) or preserved in an external backup before removal (categories C, D, E).

## 1. Verified state before cleanup

- Main HEAD: `a144ed0`
- Dirty entries: 47 (12 modified-tracked, 35 untracked)
- Worktrees: 11, all intact, none touched by this audit
- Pre-existing backup verified: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/` — every one of the 47 currently-dirty files was compared byte-for-byte (SHA-256, computed directly from each file's actual full path) against its counterpart there; all 47 matched exactly (`backup_matches: true` for every entry). The backup's remaining 17 entries correspond to files that were already resolved (tracked or removed) before this audit began and are outside today's 47-entry scope.
  - Note: running `sha256sum -c SHA256SUMS.txt` literally, from inside that backup directory, reports 62 `FAILED open or read` lines. This is a cosmetic defect in that manifest file, not a data-integrity problem: its recorded filenames are bare (`neogeocolor.png`) while the actual PNGs live one level down, in `platforms/neogeocolor.png`. Re-running the same check with `platforms/` prefixed onto every image entry (leaving the two top-level non-image entries as-is) verifies all 64 listed files with zero failures. This audit's own SHA-256 comparisons in the paragraph above were computed directly against each file's real path and are unaffected by that pre-existing manifest's path prefix issue; they are the authoritative verification for this cleanup, not the older manifest's own self-check.

## 2. Methodology

- **Dirty-entry enumeration**: `git -C /home/davedap/archivefs status --porcelain=v1 -z` (NUL-separated, so filenames containing spaces — e.g. `Atari ST.png` — are handled exactly, not split or misquoted).
- **Hashing**: SHA-256 over the full file contents, both for the working-tree copy and, for tracked (modified) entries, for the blob at `HEAD:<path>` via `git show`.
- **Image metadata**: Pillow (`PIL.Image`) — dimensions, colour mode, and a real alpha-channel extrema check (`getchannel("A").getextrema()`), not just "mode contains A": a nominally-RGBA file whose alpha channel is uniformly 255 is reported as *not* transparent.
- **Canonical platform mapping**: `archivefs_core::canonical_platform_for_alias`, called directly from a temporary in-tree Rust test in an unrelated, already-clean worktree (`archivefs-cc-dat-sources-gui`), never in `main`. The probe file was deleted immediately after use and never committed. This is the same normalisation and alias-matching logic the running application itself uses — not a hand-written guess at what should match.
- **"Already covered" check**: cross-referenced against the exact list of `include_bytes!("../assets/platforms/*.png")` bundled-asset paths in `crates/archivefs-gui/src/main.rs` (50 filenames) and the 67 canonical platform ids in `crates/archivefs-core/src/platform/mod.rs`.
- **Category A discovery**: every dirty file's SHA-256 was compared against the SHA-256 of *every* currently-tracked file under `crates/archivefs-gui/assets/platforms/` (60 distinct tracked blob hashes), not just the tracked file at the same path. 17 dirty files turned out to be byte-for-byte identical to an already-tracked asset under a different (correctly-spelled/canonical) filename — a stronger, provable form of "safe duplicate" than a same-path comparison alone would have found.

## 3. Classification summary

| Category | Count | Action |
|---|---:|---|
| Category A — Safe duplicate | 17 | Remove from main. A byte-identical copy is already tracked under its canonical name; nothing is preserved externally because nothing is lost. |
| Category B — Superseded artwork | 12 | Remove from main. The opaque replacement is discarded from the working tree; the tracked (committed) transparent original is untouched and remains exactly as `main` already has it. Already fully preserved in the pre-existing `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/` backup, verified byte-for-byte in this audit. |
| Category C — Canonical artwork candidate | 2 | Remove from main only after external backup verification (done — see below). Preserved in `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/` for a possible future visual-review branch that fills the real gap this file's canonical platform currently has no bundled artwork for. |
| Category D — Ambiguous or orphan artwork | 14 | Remove from main only after external backup verification (done — see below). Preserved in `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/`. Needs human visual inspection or a naming decision before any future use; not suitable for direct adoption as-is. |
| Category E — Non-artwork project file | 2 | Remove from main only after external backup verification (done — see below). Preserved in `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/`. Does not belong in the tracked repository (see individual reasoning). |
| **Total** | **47** | |

## 4. Full entry-by-entry record

### Category A — Safe duplicate (17 entries)

**Recommended action:** Remove from main. A byte-identical copy is already tracked under its canonical name; nothing is preserved externally because nothing is lost.

#### `crates/archivefs-gui/assets/platforms/3DO Interactive Multiplayer.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `f6797999c92a64002c487b3323ffefed2a4d73d167f99fb10a7ae6026d28b468`
- Size: 890,613 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/3DO Interactive Multiplayer.png`
- Backup SHA-256: `f6797999c92a64002c487b3323ffefed2a4d73d167f99fb10a7ae6026d28b468`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: 3do
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/3do.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/3ds.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `cdacd7c17ccf131d312da3663abddea02ee1e737c82262ea1ec4c3652da25c86`
- Size: 976,003 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/3ds.png`
- Backup SHA-256: `cdacd7c17ccf131d312da3663abddea02ee1e737c82262ea1ec4c3652da25c86`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: nintendo3ds
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/nintendo3ds.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/Atari ST.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `fbd86b26211a593a1c4f8e0e63c078b2d2b671366ba20e16f0191908549e7dbe`
- Size: 840,514 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/Atari ST.png`
- Backup SHA-256: `fbd86b26211a593a1c4f8e0e63c078b2d2b671366ba20e16f0191908549e7dbe`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: atarist
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/atarist.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/ColecoVision.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `3ec55410e8cca2b55a3a1d529b9d93d511f7055a96e23163f2734c9544a72c58`
- Size: 939,288 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/ColecoVision.png`
- Backup SHA-256: `3ec55410e8cca2b55a3a1d529b9d93d511f7055a96e23163f2734c9544a72c58`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: colecovision
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/colecovision.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/apple2.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `b2bab266c832255c899777a1b7551343df6bd584bea18afb7cbf647c3394fe4c`
- Size: 920,734 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/apple2.png`
- Backup SHA-256: `b2bab266c832255c899777a1b7551343df6bd584bea18afb7cbf647c3394fe4c`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: appleii
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/appleii.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/atrai7800.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `cf9913324472270d810869c7f0fb83d185dee059e02e20b9726da07f12d9b491`
- Size: 848,450 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/atrai7800.png`
- Backup SHA-256: `cf9913324472270d810869c7f0fb83d185dee059e02e20b9726da07f12d9b491`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: atari7800
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/atari7800.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/atria5200.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `b9cedda07108eca7475cffa2ca5041c2fb0b50fe10c93b4ecdadefdaadbc704f`
- Size: 853,688 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/atria5200.png`
- Backup SHA-256: `b9cedda07108eca7475cffa2ca5041c2fb0b50fe10c93b4ecdadefdaadbc704f`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: atari5200
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/atari5200.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/atrii2600.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `21d4c5ad9a690c57c6e0b1855f0ee47e39749f406d51650afa616111b07ad3c7`
- Size: 984,116 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/atrii2600.png`
- Backup SHA-256: `21d4c5ad9a690c57c6e0b1855f0ee47e39749f406d51650afa616111b07ad3c7`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: atari2600
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/atari2600.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/bbcmodelb.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `ad012422fda1f54bff7c7c49cdef88c007f7ba9ff9f80236ec0f6c6458a3357e`
- Size: 904,136 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/bbcmodelb.png`
- Backup SHA-256: `ad012422fda1f54bff7c7c49cdef88c007f7ba9ff9f80236ec0f6c6458a3357e`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: bbcmicro
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/bbcmicro.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/commidore64.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `a6ee50567b681cf0cf54b3fc0d92bd796277826a4308557d1f7133a27632c023`
- Size: 844,206 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/commidore64.png`
- Backup SHA-256: `a6ee50567b681cf0cf54b3fc0d92bd796277826a4308557d1f7133a27632c023`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: commodore64
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/commodore64.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/electron.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `faa8b25d7598ef5c5bad575ad77bf52e6d0d1d5d5a85e387db77b30a245f45e9`
- Size: 870,857 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/electron.png`
- Backup SHA-256: `faa8b25d7598ef5c5bad575ad77bf52e6d0d1d5d5a85e387db77b30a245f45e9`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: acornelectron
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/acornelectron.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/gba.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `318feec250ab29b2f59c5a0db9894f8369b7d88be4102ecac4e3af2eb4dd32ce`
- Size: 814,705 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/gba.png`
- Backup SHA-256: `318feec250ab29b2f59c5a0db9894f8369b7d88be4102ecac4e3af2eb4dd32ce`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: gameboyadvance
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/gameboyadvance.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/philpscdi.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `4410faad5dcf3a809153efd4e645ae13d5d43ede37eaff0371f1f8076756e71d`
- Size: 764,198 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/philpscdi.png`
- Backup SHA-256: `4410faad5dcf3a809153efd4e645ae13d5d43ede37eaff0371f1f8076756e71d`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: philipscdi
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/philipscdi.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/scumvm.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `2d878bb4917a7f2d735142d5de88750b64dc483284e7f2a1d4aedf41eaa6213c`
- Size: 1,114,227 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/scumvm.png`
- Backup SHA-256: `2d878bb4917a7f2d735142d5de88750b64dc483284e7f2a1d4aedf41eaa6213c`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: scummvm
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/scummvm.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/spectrum128k.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `c2c0cec59fb04dc3fe67c9e6a37c747db044f6ad39ebcf09f59d6f2a71e0a5df`
- Size: 902,197 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/spectrum128k.png`
- Backup SHA-256: `c2c0cec59fb04dc3fe67c9e6a37c747db044f6ad39ebcf09f59d6f2a71e0a5df`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: zxspectrum
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/zxspectrum.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/turbogratx16.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `40c0be3b0f04a51af8ed4dd3e0359ad6bcd8c797b410a896e0d79c51706017d0`
- Size: 854,602 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/turbogratx16.png`
- Backup SHA-256: `40c0be3b0f04a51af8ed4dd3e0359ad6bcd8c797b410a896e0d79c51706017d0`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: turbografx16
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/turbografx16.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

#### `crates/archivefs-gui/assets/platforms/vita.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `b21a9551c302d95e3cd306e8be241d2ed15fc8be820757cb896c8e33bcbab5f6`
- Size: 804,056 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/vita.png`
- Backup SHA-256: `b21a9551c302d95e3cd306e8be241d2ed15fc8be820757cb896c8e33bcbab5f6`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: playstationvita
- Classification: **A**
- Reason: Byte-for-byte identical (SHA-256 match) to the already-tracked crates/archivefs-gui/assets/platforms/playstationvita.png. A perfect duplicate already exists in the repository under its canonical name; nothing is lost by removing this copy.

### Category B — Superseded artwork (12 entries)

**Recommended action:** Remove from main. The opaque replacement is discarded from the working tree; the tracked (committed) transparent original is untouched and remains exactly as `main` already has it. Already fully preserved in the pre-existing `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/` backup, verified byte-for-byte in this audit.

#### `crates/archivefs-gui/assets/platforms/dreamcast.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `ee9fc627ed65b17c9f86a9249c32a5946407dc3fe74c8db1bf6a0a2857e10a7f`
- Size: 757,545 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `1e6b3cfa5914af8e6fca0fab454520c6439e7c3605fb94dc5bcb4a0bbf6e06e9`, 1,597,369 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/dreamcast.png`
- Backup SHA-256: `ee9fc627ed65b17c9f86a9249c32a5946407dc3fe74c8db1bf6a0a2857e10a7f`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Dreamcast
- Classification: **B**
- Reason: Opaque 757,545-byte RGB image overwrites the tracked, transparent (RGBA) 1,597,369-byte original bundled via include_bytes!. Same 1024x1024 canvas, different content.

#### `crates/archivefs-gui/assets/platforms/gameboy.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `3433e2fc87737621e37caf79c383a25eef7c327c1aa6cda8ae16b22607171ed2`
- Size: 875,124 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `cc8dd0a0cdc84d50817ae112107d01623bcc910e8606fd9137da3208c743666b`, 1,576,726 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/gameboy.png`
- Backup SHA-256: `3433e2fc87737621e37caf79c383a25eef7c327c1aa6cda8ae16b22607171ed2`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Game Boy
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/gamecube.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `a5b4e0b3c9a1a7db4cf1b6d7db96b9c1ca564b07208c8d4849f3031c4e2112c0`
- Size: 842,028 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `52ddc9cae8aa166ad2bbcd3d114d78b492bb000806d6b96eaecc73e547c95a7f`, 1,594,670 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/gamecube.png`
- Backup SHA-256: `a5b4e0b3c9a1a7db4cf1b6d7db96b9c1ca564b07208c8d4849f3031c4e2112c0`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: GameCube
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/megadrive.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `e76bfc55de721eaeb849eb47ae7973a089ea6ecb7e9833fab53a6ce745b5a025`
- Size: 802,388 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `01877998a23b4f77358f1ba4ec2c7fa2f27ba422e888fe00ce1a3cb4f22cc765`, 1,555,855 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/megadrive.png`
- Backup SHA-256: `e76bfc55de721eaeb849eb47ae7973a089ea6ecb7e9833fab53a6ce745b5a025`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: MegaDrive
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/n64.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `7a0e263b15c7b21a486359ec6e6963c1dc25884bb0deb1cd490cd29f81a21755`
- Size: 802,347 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `9bdad476b64b00af9adac4a84d0571f7e8cef86a986f4d6febf2f5b8aaec8fb8`, 1,571,533 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/n64.png`
- Backup SHA-256: `7a0e263b15c7b21a486359ec6e6963c1dc25884bb0deb1cd490cd29f81a21755`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: N64
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/saturn.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `071127acfe315995a5ffe6864617a0b7f5401e7cca2b20e88c6c0bd2eb706808`
- Size: 927,140 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `6f0d7e0720a4bd29067925ab42a9a82869fe6bbd60ff9a37ba606c3e98698f46`, 1,537,579 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/saturn.png`
- Backup SHA-256: `071127acfe315995a5ffe6864617a0b7f5401e7cca2b20e88c6c0bd2eb706808`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Saturn
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/snes.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `5380818cd38f84aa5b05cb5abfb36b88323b563f0e097d476289764cbd0efbe9`
- Size: 881,030 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `384050c4fc92088d38b4d0791bfc499c44483eac146602668f8c1917d598e62c`, 1,633,370 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/snes.png`
- Backup SHA-256: `5380818cd38f84aa5b05cb5abfb36b88323b563f0e097d476289764cbd0efbe9`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: SNES
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/switch.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `16707ff47ebf5391852600483d11aeb05a0eb766f432b4ec6936a310319ea08b`
- Size: 696,768 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `ee3cf874991397f9adfd04de5ece9b843caeb62eee6a999785967578373488cc`, 1,509,672 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/switch.png`
- Backup SHA-256: `16707ff47ebf5391852600483d11aeb05a0eb766f432b4ec6936a310319ea08b`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Switch
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/wii.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `3abe144ccb4101f0891db4dfda7c6bdc4ecb189f049f0c6d62d9526593d27de0`
- Size: 726,350 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `c91ff09181ac9f9f42ab10682257b88862aaa127a6edfede3937ac629127bc40`, 1,489,732 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/wii.png`
- Backup SHA-256: `3abe144ccb4101f0891db4dfda7c6bdc4ecb189f049f0c6d62d9526593d27de0`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Wii
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/wiiu.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `60f195aabe36d14e36b10291a9a61bd8c032e52af2056f76094fb15ac1620272`
- Size: 697,270 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `d1f4dbc144b8a88d0ffdd397671e7588befc9684a5b9516ae5ea618d4c4ec049`, 1,641,337 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/wiiu.png`
- Backup SHA-256: `60f195aabe36d14e36b10291a9a61bd8c032e52af2056f76094fb15ac1620272`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: WiiU
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/xbox.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `d15af1089db8cb5ae2d47aa78e9cb2f3326c49b99b735b0e11839593f3417988`
- Size: 752,500 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `227c7bd000d7e7dadecb5541e23b6b1d01f04af531513dd62845a2d5762376ec`, 1,582,929 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/xbox.png`
- Backup SHA-256: `d15af1089db8cb5ae2d47aa78e9cb2f3326c49b99b735b0e11839593f3417988`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Xbox
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

#### `crates/archivefs-gui/assets/platforms/xbox360.png`
- Tracked/untracked status: modified(tracked)
- SHA-256 (working tree): `e72ba0790bd68c234effc57bddc04ac99b54e22f7e21454187f836a3faaf6956`
- Size: 761,771 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Tracked HEAD blob at this path: SHA-256 `7402a69a9d69dafff61de4d77227343d9cf3dda8cf22ab74173f4142fd7325df`, 1,533,459 bytes, 1024x1024 RGBA (transparent: yes)
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/xbox360.png`
- Backup SHA-256: `e72ba0790bd68c234effc57bddc04ac99b54e22f7e21454187f836a3faaf6956`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Xbox360
- Classification: **B**
- Reason: Opaque RGB overwrite of the tracked transparent RGBA original bundled via include_bytes!.

### Category C — Canonical artwork candidate (2 entries)

**Recommended action:** Remove from main only after external backup verification (done — see below). Preserved in `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/` for a possible future visual-review branch that fills the real gap this file's canonical platform currently has no bundled artwork for.

#### `crates/archivefs-gui/assets/platforms/supergratx.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `cc47b0ed550576b69aac6f8acbd3d7a53bafc2ef7edec37bbab1c071e2335da7`
- Size: 986,817 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/supergratx.png`
- Backup SHA-256: `cc47b0ed550576b69aac6f8acbd3d7a53bafc2ef7edec37bbab1c071e2335da7`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: PC Engine
- Classification: **C**
- Reason: 'supergratx' resolves via the registry's own 'necpcenginesupergrafx' folder alias family for the 'PC Engine' canonical id (SuperGrafx is a PC Engine variant, not a separate id). PC Engine currently has NO bundled artwork anywhere in the GUI. Filename has a letter-transposition typo ('gratx' for 'grafx') that should be corrected before any future adoption.

#### `crates/archivefs-gui/assets/platforms/turbogratxcd.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `3600edb5282b649d1b9652efdbaf586516acedcc949085bc8381fbea00eeb060`
- Size: 852,468 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/turbogratxcd.png`
- Backup SHA-256: `3600edb5282b649d1b9652efdbaf586516acedcc949085bc8381fbea00eeb060`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: PC Engine CD
- Classification: **C**
- Reason: Real-world 'TurboGrafx-CD' is the US name for the 'PC Engine CD' canonical id, which currently has NO bundled artwork anywhere in the GUI. Filename has the same letter-transposition typo as turbogratx16.png and should be corrected before any future adoption.

### Category D — Ambiguous or orphan artwork (14 entries)

**Recommended action:** Remove from main only after external backup verification (done — see below). Preserved in `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/`. Needs human visual inspection or a naming decision before any future use; not suitable for direct adoption as-is.

#### `crates/archivefs-gui/assets/platforms/4448b039-69a6-4690-a61f-dfc5393c3069.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `ef3f2d13d780f1b180d09a27c9bbd845fbdbc98e4d763459256ed74faca27427`
- Size: 878,502 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/4448b039-69a6-4690-a61f-dfc5393c3069.png`
- Backup SHA-256: `ef3f2d13d780f1b180d09a27c9bbd845fbdbc98e4d763459256ed74faca27427`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: none (accidental UUID filename)
- Classification: **D**
- Reason: Filename is a raw UUID with no platform meaning at all. No canonical id can be inferred from the name; would need visual inspection to identify the depicted platform, if any.

#### `crates/archivefs-gui/assets/platforms/Archimedes.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `59c02686cea495152b82e733ca793c1d52ebc1af3030920b7b4ea1139f6fde4e`
- Size: 1,002,504 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/Archimedes.png`
- Backup SHA-256: `59c02686cea495152b82e733ca793c1d52ebc1af3030920b7b4ea1139f6fde4e`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Acorn Archimedes
- Classification: **D**
- Reason: Resolves exactly, but acornarchimedes.png is already bundled. Redundant duplicate.

#### `crates/archivefs-gui/assets/platforms/dragon.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `d694d183adfda3efccd6b741f58d389f1bb6a6c1e4c60a42d22b1a0dbd55c76f`
- Size: 1,270,129 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/dragon.png`
- Backup SHA-256: `d694d183adfda3efccd6b741f58d389f1bb6a6c1e4c60a42d22b1a0dbd55c76f`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: unsupported (no 'Dragon' canonical id)
- Classification: **D**
- Reason: The Dragon 32/64 is not a registered canonical platform anywhere in platform/mod.rs. No mapping exists in this codebase.

#### `crates/archivefs-gui/assets/platforms/dreamcatst.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `a26d097dcb10d2dfeeb7761b4f26442af14323f3726e8b77dcacc7671f86ad1a`
- Size: 1,601,393 bytes
- Image: 1024x1024, mode RGBA, transparent: yes
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/dreamcatst.png`
- Backup SHA-256: `a26d097dcb10d2dfeeb7761b4f26442af14323f3726e8b77dcacc7671f86ad1a`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: Dreamcast (probable, but see note)
- Classification: **D**
- Reason: 'dreamcatst' does not match any registered alias (transposed-letter typo for 'dreamcast'). NOTABLE: unlike every other untracked candidate, this file IS RGBA with genuine transparency (1,601,393 bytes) - the same shape as the *correct*, now-overwritten tracked dreamcast.png used to be. Strong circumstantial evidence this is the intended replacement artwork, misnamed by a typo, while a different and opaque image was separately (and wrongly) written into the correctly-spelled dreamcast.png slot (see the B-category entry above). Flagged for human visual comparison before any rename is considered - this cleanup task does not perform that content decision.

#### `crates/archivefs-gui/assets/platforms/neogeocolor.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `9ff0628f3e35bff9f880de300ae8d5a98de3562c2e7f29ccd42f0396f0c1ef9d`
- Size: 825,968 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/neogeocolor.png`
- Backup SHA-256: `9ff0628f3e35bff9f880de300ae8d5a98de3562c2e7f29ccd42f0396f0c1ef9d`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: ambiguous ('Neo Geo Pocket Color' vs recolored NeoGeo)
- Classification: **D**
- Reason: 'neogeocolor' does not match 'Neo Geo Pocket Color's registered aliases (neogeopocketcolor/ngpc/snkneogeopocketcolor - all require 'pocket'). Could plausibly be intended for that uncovered platform, or could be an alternate NeoGeo (already-bundled) rendition. Needs visual inspection before it can be called a genuine gap-filler.

#### `crates/archivefs-gui/assets/platforms/pcbox.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `28a8f41f8f110b278dc21d0c0f25b49befb21e3eb88e9467dfe2d33e81cbe090`
- Size: 883,703 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/pcbox.png`
- Backup SHA-256: `28a8f41f8f110b278dc21d0c0f25b49befb21e3eb88e9467dfe2d33e81cbe090`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: ambiguous ('PC' vs 'PC Engine', both uncovered)
- Classification: **D**
- Reason: 'pcbox' matches no registered alias. Could plausibly be intended for the 'PC' (DOS-class) canonical id or a generic PC Engine box shot; both currently have no bundled artwork, but the filename alone does not disambiguate which. Needs visual inspection.

#### `crates/archivefs-gui/assets/platforms/pcfx.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `c12765de053ce408c9c9e0a4bae4c59338a86ec4ccf6a852905e84c2b6546b55`
- Size: 829,964 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/pcfx.png`
- Backup SHA-256: `c12765de053ce408c9c9e0a4bae4c59338a86ec4ccf6a852905e84c2b6546b55`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: unsupported (no 'PC-FX' canonical id)
- Classification: **D**
- Reason: PC-FX is not a registered canonical platform in this codebase (only 'PC Engine' and 'PC Engine CD' exist for the NEC family).

#### `crates/archivefs-gui/assets/platforms/psx2.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `e3c7cb8c2d5243dc4eeff26d9cbfe3cdfa9c5ade5ff4a4d9ee2d88ded3b2f13b`
- Size: 837,234 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/psx2.png`
- Backup SHA-256: `e3c7cb8c2d5243dc4eeff26d9cbfe3cdfa9c5ade5ff4a4d9ee2d88ded3b2f13b`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: ambiguous/unclear (numbered variant, no registered meaning)
- Classification: **D**
- Reason: No registered alias for 'psx2'. Not a recognised way to name PS2 (which already has a distinct bundled ps2.png); reads as an alternate/duplicate render of PSX artwork or a mislabeling attempt. Needs visual inspection.

#### `crates/archivefs-gui/assets/platforms/psx3.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `66102e6c6f77ed00e3ad56ea2e673fc53913d6ca44c5d759bafa85284aaa8839`
- Size: 818,227 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/psx3.png`
- Backup SHA-256: `66102e6c6f77ed00e3ad56ea2e673fc53913d6ca44c5d759bafa85284aaa8839`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: ambiguous/unclear (numbered variant, no registered meaning)
- Classification: **D**
- Reason: Same reasoning as psx2.png; PS3 already has a distinct bundled ps3.png.

#### `crates/archivefs-gui/assets/platforms/psx4.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `af8533c8ccc206883932793c799f700b4184d9ad83815e3be224b3e7c2b6642f`
- Size: 729,029 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/psx4.png`
- Backup SHA-256: `af8533c8ccc206883932793c799f700b4184d9ad83815e3be224b3e7c2b6642f`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: ambiguous/unclear; would need an unsupported 'PS4' id
- Classification: **D**
- Reason: No registered alias for 'psx4', and 'PS4' is not a canonical platform in this codebase at all.

#### `crates/archivefs-gui/assets/platforms/psx5.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `66604e9af00943548c4d156220340133fa9aca8250f56d55c71fc59063a4fa23`
- Size: 635,933 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/psx5.png`
- Backup SHA-256: `66604e9af00943548c4d156220340133fa9aca8250f56d55c71fc59063a4fa23`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: ambiguous/unclear; would need an unsupported 'PS5' id
- Classification: **D**
- Reason: No registered alias for 'psx5', and 'PS5' is not a canonical platform in this codebase at all.

#### `crates/archivefs-gui/assets/platforms/tandycomputer.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `e4b9ab2dea4523bc70f4aaec01a1eb9f9dbb55a7ebc387f5ea44881e1e8710b3`
- Size: 1,075,839 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/tandycomputer.png`
- Backup SHA-256: `e4b9ab2dea4523bc70f4aaec01a1eb9f9dbb55a7ebc387f5ea44881e1e8710b3`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: unsupported (no 'Tandy' canonical id)
- Classification: **D**
- Reason: Tandy/TRS-80 CoCo is not a registered canonical platform. No mapping exists in this codebase.

#### `crates/archivefs-gui/assets/platforms/vc4000.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `d892a98cd823e7ebaf8db93135804c552ff98f796117dd361c1f236a06674941`
- Size: 884,169 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/vc4000.png`
- Backup SHA-256: `d892a98cd823e7ebaf8db93135804c552ff98f796117dd361c1f236a06674941`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: unsupported (no 'Interton VC 4000' canonical id)
- Classification: **D**
- Reason: Not a registered canonical platform. No mapping exists in this codebase.

#### `crates/archivefs-gui/assets/platforms/xboxone.png`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `566784d985d10d01dd09a8a0bc344dd16002ff89b01905ec6b01d3051d66f301`
- Size: 739,918 bytes
- Image: 1024x1024, mode RGB, transparent: no
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/platforms/xboxone.png`
- Backup SHA-256: `566784d985d10d01dd09a8a0bc344dd16002ff89b01905ec6b01d3051d66f301`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: unsupported (no 'Xbox One' canonical id)
- Classification: **D**
- Reason: Only 'Xbox' and 'Xbox360' are registered canonical platforms; there is no distinct 'Xbox One' id in this codebase.

### Category E — Non-artwork project file (2 entries)

**Recommended action:** Remove from main only after external backup verification (done — see below). Preserved in `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/`. Does not belong in the tracked repository (see individual reasoning).

#### `RETROARCH_ENV_DESIGN_DRAFT_v1.md`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `ad48822ffe8f9b9890956b03624ed3b93cb034a8f78cdb17aa79582f15cf7ec5`
- Size: 54,796 bytes
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/RETROARCH_ENV_DESIGN_DRAFT_v1.md`
- Backup SHA-256: `ad48822ffe8f9b9890956b03624ed3b93cb034a8f78cdb17aa79582f15cf7ec5`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: n/a
- Classification: **E**
- Reason: Pre-review design-research draft (explicitly self-labelled 'v1, pre-review') that predates and fed into the RetroArch environment-discovery feature, which has since shipped in full with its own authoritative documentation at docs/RETROARCH_ENVIRONMENT.md (5 feature commits, 299-line doc). This draft's content is superseded; no repository directory (docs/design, docs/adr, docs/reviews) is a natural fit for a superseded pre-implementation scratch document, and there is no docs/archive/ convention in this repository. Recommend: external backup only, not the repository.

#### `send-to-nobara.sh`
- Tracked/untracked status: untracked
- SHA-256 (working tree): `f9f7027d230ae77f317efd657d513879b94e9ca944d134a997c22552ec07d544`
- Size: 626 bytes
- Backup path: `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/send-to-nobara.sh`
- Backup SHA-256: `f9f7027d230ae77f317efd657d513879b94e9ca944d134a997c22552ec07d544`
- Backup matches working tree byte-for-byte: yes
- Canonical platform mapping: n/a
- Classification: **E**
- Reason: Ad-hoc personal deployment script: hardcodes the author's home IP address (192.168.1.51), username, and absolute local paths. scripts/test-on-nobara.sh already provides a proper, parameterized (NOBARA_HOST env var), safer superset of this workflow (build + fmt + clippy + test + bundle + verify + remote smoke-test), already tracked and used by the project. This script is redundant with existing project tooling and embeds what should be treated as personal/private network information. Recommend: outside the repository, personal backup only - do not commit.

## 5. External preservation

Category C, D and E files (18 total: 2 C + 14 D + 2 E) were copied byte-for-byte to `/home/davedap/archivefs-cleanup-backups/2026-08-05-release-leftovers/` before any removal from `main`. Verified twice, independently:
1. Inline SHA-256 comparison during the copy itself (source == destination == recorded working-tree hash), enforced with a hard assertion.
2. A separate `cmp` pass over every copied pair after the fact, and a `sha256sum -c` check of the backup's own `SHA256SUMS.txt` — both passed for all 18 files with zero mismatches.

Category A and B files were **not** copied into this new backup: A is redundant with content already tracked on `main` (recoverable from `git show HEAD:<canonical path>` at any time), and B is already fully preserved, and already verified in §1, in the pre-existing `/home/davedap/archivefs-cleanup-backups/2026-08-05-platform-artwork-main/` backup.

## 6. Notable finding requiring human judgement

**`dreamcatst.png`** is the one Category D image with genuine alpha transparency (RGBA, 1,601,393 bytes) among all 35 untracked candidates — every other untracked PNG is flat opaque RGB. Its filename is a letter-transposition typo of `dreamcast` (the same failure mode seen in `atrai7800.png`, `atria5200.png`, `atrii2600.png`, `turbogratx16.png`, and others in this set). This is circumstantial but suggestive: it is plausible that this file is the *correct*, intended transparent Dreamcast artwork, saved under a typo'd name, while a separate and unrelated opaque image was written into the correctly-spelled, tracked `dreamcast.png` slot (Category B, §4) — overwriting the good transparent original that `git show HEAD:.../dreamcast.png` still holds untouched. This audit does not act on that hypothesis: comparing the two images visually and deciding whether to adopt `dreamcatst.png` (renamed) as the real `dreamcast.png` is a content decision for a human or a dedicated visual-review branch, not a cleanup task. Both images are preserved — the correct tracked original in `main` itself (never touched by this audit) and `dreamcatst.png` in the release-leftovers backup.

## 7. Deferred items requiring human judgement

- **`RETROARCH_ENV_DESIGN_DRAFT_v1.md`**: judged not to belong in the tracked repository, since it is a self-labelled "v1, pre-review" research draft superseded by the shipped `docs/RETROARCH_ENVIRONMENT.md` (299 lines, 5 feature commits). No existing repository convention (`docs/design/`, `docs/adr/`, `docs/reviews/`) fits a superseded pre-implementation scratch document, and there is no `docs/archive/` directory in this repository. A maintainer may reasonably disagree and decide it has historical/process value worth adding under a new `docs/archive/` convention — that call was left to a human rather than made unilaterally here.
- **`send-to-nobara.sh`**: judged not to belong in the tracked repository, since `scripts/test-on-nobara.sh` already provides a proper, parameterised, safer superset of the same workflow, and this script hardcodes the author's home IP address, username, and absolute local paths. A maintainer should confirm whether that IP address is sensitive enough to warrant scrubbing from the external backup copy too, or whether it is inconsequential (private LAN address, not reachable from outside the author's own network).
- **Category C candidates** (`supergratx.png` → PC Engine, `turbogratxcd.png` → PC Engine CD): both fill genuine gaps (no bundled artwork exists today for either platform), but both need their filename typos corrected and their content visually confirmed as actually depicting the right hardware before any future adoption branch uses them.
- **Category D ambiguous entries** (`pcbox.png`, `neogeocolor.png`, `psx2.png`–`psx5.png`, `Archimedes.png`, `dreamcatst.png`): each needs a human to open the image and decide what it actually depicts before any classification stronger than "ambiguous" can be assigned.

## 8. Result

- Files removed from `main`: 29 (17 Category A + 12 Category B).
- Files preserved externally then removed from `main`: 18 (2 C + 14 D + 2 E).
- Total removed from `main`: 47 — matching the original dirty count exactly.
- `git status --short` in `/home/davedap/archivefs` after cleanup: empty.
- No tracked file content changed: every removal was of an untracked file or a reversion of a modified-but-uncommitted tracked file back to its committed `HEAD` content (`git restore`), never a new commit and never a change to what `origin/main` or any other worktree has.

---

*Audit performed against main HEAD `a144ed0` on 2026-08-05. This document itself was written to keep `main` from becoming dirty again — see the accompanying report for where it was committed, if anywhere.*
