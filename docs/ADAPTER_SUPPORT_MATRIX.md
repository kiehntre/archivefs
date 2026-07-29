# Cheats & Mods adapter support matrix

A concise, current-state summary of what each Cheats & Mods emulator
adapter can actually do. See
[`docs/CHEATS_MODS_SAFETY.md`](CHEATS_MODS_SAFETY.md) and
[`docs/CHEATS_MODS_USER_POLICY.md`](CHEATS_MODS_USER_POLICY.md) for the
trust and safety model behind every row below, and the per-adapter
documents linked in each row for full detail.

## Cheats, patches, and mods are not the same thing

- **Cheats** are small, targeted value changes (a RetroArch `.cht` entry,
  a Dolphin `[ActionReplay]`/`[Gecko]` code) - the only category any
  adapter below can currently install.
- **Patches** are broader per-game fixes distributed as files (a PCSX2
  `.pnach` can contain a widescreen-hack patch alongside cheat codes in
  the same file; RetroArch also has separate soft-patch formats). ArchiveFS
  inspects these the same conservative way it inspects cheats - as inert,
  unevaluated data - but does not yet distinguish "patch" from "cheat" as
  a first-class category anywhere in the GUI.
- **Mods** (textures, graphics packs, resource packs, Riivolution assets,
  and similar larger content replacements) are **not implemented in any
  form**. No mod adapter exists, no mod inspection exists, and the "Mods"
  section of the Cheats & Mods workspace is a labelled placeholder only.

## Matrix

| Adapter | Identity | Local inspection | Trusted provider | Preview | Apply | Backup | Rollback | Mods | Current blocker |
|---|---|---|---|---|---|---|---|---|---|
| **RetroArch** | Verified (exact/strong trusted-catalogue match bound to archive evidence) | N/A - the source is the trusted catalogue itself, not an emulator-managed directory to inventory | **Yes** - reviewed `libretro-database` provider, Download/Update/Verify from Sources, immutable content-addressed snapshots | Yes, shared model | **Yes** - explicit confirmation, separate replacement approval, background execution | Yes - verified, never-overwritten backup before any replacement | Yes - fresh preview re-derived before acting, blocks on user-modified content or a missing/changed backup | Not implemented | None known - real manual Nobara QA against a live-fetched catalogue is the remaining pre-tag step (see `docs/V0.6_RELEASE_AUDIT.md`) |
| **PCSX2** | Verified (PCSX2 executable CRC via the shared bounded ISO reader) | Yes - inventories existing `cheats`/`cheats_ws`/`patches` directories and `.pnach` files, read-only | No - no official-provider integration exists yet; only emulator-managed local files are inspected | Yes, shared model | **No** - no independent, approved source artifact to apply from; this is a scope decision, not a bug | N/A (no apply) | N/A (no apply) | Not implemented | No official `pcsx2_patches` provider yet; no per-section selection or safe PNACH merge yet |
| **Dolphin** | Verified Game ID and revision via the shared bounded ISO/ZIP reader | Yes - inventories optional existing `GameSettings/*.ini` files | **Yes** - exact-ID lookup in the official Dolphin upstream GameSettings dataset, with bounded retrieval and local cache | Yes, including individual Gecko-code selection, complete generated sections, preserved sections, and final hash | **Yes** - safely merges `[Gecko]` and `[Gecko_Enabled]` after explicit preview/confirmation; creates the exact INI when absent | Yes - verified, never-overwritten backup before replacement | Yes - restores exact previous bytes or removes an INI created by the transaction | Not implemented | Upstream does not declare per-code disc revisions, so uncertain applicability is always labelled |
| **Xenia (Xbox 360)** | Verified Title ID and Media ID from the unencrypted XEX2 module header (direct `.xex` or a single `.xex` inside a ZIP) | N/A - no local bulk inventory; the exact destination file is checked individually once a candidate is chosen | **Yes** - exact-Title-ID lookup in `xenia-canary/game-patches`, via a cached, revision-pinned repository index (never a full clone per lookup) and per-file immutable caching | Yes, shared model, including individual patch selection, the exact merged `.patch.toml`, preserved unrelated definitions, and final hash | **Yes** - merges selected patches into the exact destination `.patch.toml` after explicit preview/confirmation; a `PartiallyVerified` candidate (module hash never computed/verified by ArchiveFS) additionally requires an explicit acknowledgement before it can be staged at all | Yes - verified, never-overwritten backup before replacement | Yes - restores exact previous bytes or removes a file created by the transaction | Not implemented | See [`XENIA_PROVIDER.md`](XENIA_PROVIDER.md) for the full identity/compatibility model; no native Xenia Canary install path is auto-detected (explicit directory only) |

## Notes on "Apply" specifically

"Apply" above means: the GUI offers a real Install/Replace control backed
by the shared safe-apply transaction engine (atomic write, verified
backup before replacement, journal, and rollback). It does **not** mean
"ArchiveFS runs the cheat" - ArchiveFS never executes cheat, patch, or mod
content at any stage, for any adapter, in preview, apply, or rollback.
"No Apply" for PCSX2 means exactly that: it remains read-only. Dolphin's
reviewed Gecko path can merge selected upstream definitions into an existing
exact-ID GameSettings file or create that file when absent. Xenia's reviewed
patch path does the same for `.patch.toml`. Neither installs other mod types.

## Snapshot vs. individual-entry trust

A **Trusted** provider (RetroArch's `libretro-database`) means ArchiveFS
has reviewed the source's ownership, format, host, and retrieval limits -
it does **not** mean every individual cheat entry inside that catalogue
has been reviewed for correctness. An entry can be structurally valid and
still simply not work as expected in a given game/region/revision; that is
a catalogue-content question, separate from ArchiveFS's own trust and
safety guarantees.
