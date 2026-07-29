# PCSX2 cheat adapter and safe PNACH workflow

This milestone adds an install-capable PCSX2 core adapter without changing the
current Cheats & Mods screen composition. It converts a verified PlayStation 2
identity and explicitly selected, approved provider records into one staged
CRC-named PNACH, then delegates preview, confirmation, apply, backup, journal,
verification, and Undo to ArchiveFS's shared transaction engine.

No provider download is bundled. A GUI or provider integration must construct
an approved `Pcsx2CheatProviderCatalogue` from a separately reviewed source.
This is intentional: ArchiveFS does not invent codes or treat a community
collection as trusted merely because it exists.

## PCSX2 evidence and assumptions

The implementation follows PCSX2 repository evidence rather than guessing:

- PCSX2's FAQ identifies PNACH as its cheat/patch format:
  <https://github.com/PCSX2/pcsx2/blob/master/pcsx2/Docs/PCSX2_FAQ.md>.
- A PCSX2 repository issue demonstrates an eight-hex-digit executable CRC
  filename such as `0E7F91DA.pnach`, a plaintext `patch=` directive, and the
  separate `cheats` and `cheats_ws` activation choices:
  <https://github.com/PCSX2/pcsx2/issues/627>.
- The official `pcsx2_patches` repository describes itself as the distribution
  source for widescreen, no-interlace, and similar patches, not as an ordinary
  cheat catalogue: <https://github.com/PCSX2/pcsx2_patches>.
- The emulator source of record is <https://github.com/PCSX2/pcsx2>.

The resulting assumptions are deliberately narrow:

- The normal-cheat destination is `<confirmed profile>/cheats/<CRC>.pnach`.
- `<CRC>` is exactly eight hexadecimal characters and is rendered uppercase.
- `cheats_ws` is a different PCSX2 feature. It may be inventoried read-only,
  but this adapter never selects it as an install destination.
- Only plaintext `patch=` entries in the implemented grammar are accepted.
  Encrypted cheat formats are rejected; ArchiveFS performs no conversion.
- Installing selected entries does not change PCSX2's global **Enable Cheats**
  setting. The user enables normal cheats in PCSX2; ArchiveFS neither launches
  the emulator nor changes unrelated emulator settings.
- Every selected managed block contains active PNACH `patch=` directives. No
  unselected provider record is materialized. Existing user lines, including
  disabled entries, remain byte-for-byte where they were.

## Profile discovery and choice

`discover_pcsx2_profiles` checks only bounded, known locations:

- native Linux: `$XDG_CONFIG_HOME/PCSX2` (normally `~/.config/PCSX2`);
- Flatpak user configuration:
  `~/.var/app/net.pcsx2.PCSX2/config/PCSX2`, with user/system Flatpak install
  evidence used to label the installation scope;
- portable/manual profiles supplied explicitly through
  `Pcsx2ProfileDiscoveryRoots::portable_configuration_roots`.

A directory is eligible only when it is an absolute, non-root, safe directory
with PCSX2 evidence (`PCSX2.ini` or `inis/`). Symlinked or otherwise unsafe
roots and patch directories are blocked. Discovery never recursively searches
the home directory for portable installs.

`confirmed_pcsx2_profile` returns the sole eligible profile automatically. If
there are multiple eligible profiles it returns
`Pcsx2ProfileChoiceError::ConfirmationRequired` with the exact IDs. The GUI
must present those choices and may remember the confirmed ID with the existing
generic emulator-profile memory APIs:
`remember_emulator_profile_to`, `remembered_profile_for`, and
`forget_emulator_profile_at`. A remembered choice is not silently substituted
when its exact profile ID is no longer eligible.

`pcsx2_cheats_directory` resolves only the normal `Cheats` category and accepts
an existing or safely creatable missing `cheats` directory. The shared engine
creates that one confirmed directory during apply, never during discovery,
identity, catalogue matching, staging, or preview.

## Identity and CRC truthfulness

`Pcsx2GameIdentity::from_report` adapts the existing bounded
`GameIdentityReport`. It retains title, verified PS2 serial when present,
verified executable CRC, technical evidence, selected archive path, and a
terminal `Pcsx2IdentityState`:

- `Verified`
- `MissingCrc`
- `Deferred`
- `Ambiguous`
- `Unsupported`

Only `IdentityStatus::Verified` PCSX2 executable CRC evidence is promoted.
Candidate, filename-derived, ambiguous, invalid, deferred, and unsupported
evidence cannot produce a PNACH destination. `verified_crc()` consequently
returns a value only in the verified state. Region is retained when a trusted
caller has it; this milestone does not infer region from a serial prefix.

Incomplete identity is a truthful terminal result with a beginner-facing
reason. It does not spin indefinitely and it does not claim a verified CRC.
`build_pcsx2_install_preview` also compares the current selected archive with
the identity archive and the staged CRC filename, preventing a result for game
A from appearing or applying under game B.

## Provider trust and compatibility

The provider-neutral API is:

- `Pcsx2CheatProviderCatalogue`
- `Pcsx2CheatProviderRecord`
- `build_pcsx2_cheat_candidates`
- `Pcsx2CheatCandidate`
- `Pcsx2CheatSelection`
- `selected_pcsx2_managed_cheats`

Each record exposes a stable ID, name, optional description, source/provider,
CRC, optional serial and region constraints, raw PNACH patch lines, category,
and confidence. Candidate evaluation is fail-closed. It blocks an unapproved
provider, an unverified record, duplicate IDs, incomplete game identity,
invalid or different CRC, absent or different required serial/region,
malformed directives, encrypted formats, and widescreen content. Selection is
by exact stable ID; an ID absent from the current candidate set is stale and is
rejected.

The adapter does not endorse a catalogue. Provider review must establish
ownership, licence, transport, attribution, and record verification before
setting `Pcsx2ProviderTrust::Approved` and a verified record confidence.

## PNACH parser and managed renderer

`parse_pnach_document` preserves the original UTF-8 bytes and recognizes only
ArchiveFS's deterministic comment-delimited blocks. It rejects oversized or
invalid UTF-8 input and malformed, nested, unterminated, or duplicate managed
blocks. Unknown user lines are not interpreted or normalized.

`PnachPatchLine::parse` accepts the managed subset:

`patch=<0|1|2>,<EE|IOP>,<8 hex address>,<byte|short|word|double|extended>,<hex value>`

Whitespace and case are normalized only for new managed directives. Comments,
unknown lines, unrelated cheats, disabled entries, newline style, and other
existing formatting remain unchanged. Additions are appended as:

```text
// ArchiveFS managed block: <stable cheat ID>
// <cheat name>
// <optional description>
patch=...
// End ArchiveFS managed block
```

Existing blocks with the same stable ID are rejected instead of silently
replaced. The renderer never rewrites or removes unrelated content.

## Preview, Install, and Undo

The core integration sequence is:

1. `confirmed_pcsx2_profile`
2. `Pcsx2GameIdentity::from_report`
3. `build_pcsx2_cheat_candidates`
4. `selected_pcsx2_managed_cheats`
5. `stage_pcsx2_pnach`
6. `build_pcsx2_install_preview`
7. `build_shared_transaction_plan`
8. `execute_shared_apply` with an exact plan-ID confirmation
9. `preview_shared_rollback`
10. `execute_shared_rollback` with an exact preview-ID confirmation

Staging reads an existing destination without following a final-component
symlink, merges in memory, and writes the generated source atomically to a
private staging directory. It does not modify the live profile. The preview
shows the exact destination, create/replace state, staged digest, and shared
transaction conflicts. The GUI can show `plain_summary` in Gamer View and
`technical_details` in Advanced View.

Apply revalidates the plan and filesystem state, takes a per-root lock, creates
only the confirmed `cheats` parent when necessary, backs up existing bytes,
writes through an atomic temporary-file/flush/rename sequence, verifies the
result, and records a journal. It never writes to the selected ISO, changes a
BIOS, starts PCSX2, or enables unrelated cheats.

Undo is intentionally operation-wide and state-sensitive:

- if Install created a new PNACH, Undo removes it only when its bytes still
  match that operation's installed digest;
- if Install replaced an existing PNACH, Undo restores the exact backed-up
  bytes;
- a missing/changed backup or externally changed destination blocks Undo;
- if operation B followed operation A, A cannot be undone through B. Undo B
  first, which restores A exactly, then Undo A.

This last rule preserves multiple managed blocks without inventing a second
partial-edit rollback mechanism.

## Safety tests

All automated install tests construct disposable profiles beneath the process
temporary directory. The transaction history, backup, and staging roots are
also disposable and are deliberately outside the emulated profile root. Tests
cover profile discovery and ambiguity, manual roots, CRC validation, identity
terminal states, strict PNACH parsing, preservation of unknown content,
provider/record trust, wrong-region blocking, stale selection, new-file Undo,
exact-byte replacement Undo, sequential managed blocks, missing backup,
external changes, permission denial, injected atomic-write failures, and ROM
immutability. No test consults or writes a live PCSX2 profile.

## Exact disposable manual procedure

This procedure prepares evidence without touching a live profile. It uses no
real cheat code and therefore does **not** claim PCSX2 gameplay recognition.

1. Create a disposable root:

   ```bash
   export ARCHIVEFS_PCSX2_PROOF="$(mktemp -d /tmp/archivefs-pcsx2-proof.XXXXXX)"
   mkdir -p "$ARCHIVEFS_PCSX2_PROOF/profile"
   printf '[UI]\n' > "$ARCHIVEFS_PCSX2_PROOF/profile/PCSX2.ini"
   ```

2. In a development harness, pass
   `$ARCHIVEFS_PCSX2_PROOF/profile` through
   `portable_configuration_roots` and confirm its returned profile ID.
3. Select a legally obtained supported PS2 image and inspect the identity
   report. Continue only if the serial/CRC evidence is verified and the shown
   uppercase CRC agrees with PCSX2's own game properties/log.
4. Load a separately approved catalogue and verify that the result is an exact
   CRC match (and exact serial/region match when constrained). Select exactly
   one entry.
5. Stage and review the preview. Confirm that the destination is
   `$ARCHIVEFS_PCSX2_PROOF/profile/cheats/<CRC>.pnach`, that only the selected
   block is added, and whether a backup will be created.
6. Confirm Install and inspect the resulting PNACH. Verify the journal and, for
   a replacement, the backup digest.
7. Later, and only by explicit user choice, point a disposable/manual PCSX2
   setup at that profile, enable normal cheats, and check recognition. This
   launch proof is pending GUI/provider integration; ArchiveFS never launches
   PCSX2.
8. Close PCSX2, refresh the rollback preview, confirm Undo, and verify the new
   file is removed or the prior file bytes are restored exactly.
9. Remove the disposable root when finished. It contains no ArchiveFS-managed
   live-profile state.

Never use an actual live profile for this procedure unless the user explicitly
chooses it after reviewing the preview.

## Known limitations and GUI integration

- No approved downloadable PCSX2 ordinary-cheat provider ships in this
  milestone. The provider contract is complete, but source approval and
  retrieval remain a separate integration decision.
- The active PCSX2 GUI page remains its pre-existing read-only inventory page.
  Claude Code's Gamer View work should consume the APIs listed above and reuse
  the existing async generation/token guard. No rendering function was moved
  or redesigned here.
- The GUI must clear candidates, selection, staged data, preview, install
  result, and Undo state whenever the selected archive, verified CRC, confirmed
  profile, or provider snapshot changes.
- Region is checked when trusted evidence supplies it; ArchiveFS does not infer
  a region merely from serial spelling.
- Widescreen/no-interlace management, encrypted conversion, ROM/ISO changes,
  emulator settings changes, and PCSX2 launch are out of scope.

Conflict risk with `feature/gui-navigation-reset` is low: the implementation
adds core modules/tests plus this document, changes only PCSX2 capability
routing in the shared transaction module, and does not edit
`crates/archivefs-gui/src/main.rs`. Cherry-pick the commits in branch order so
the identity/profile types precede the parser, provider, install plan, and
end-to-end tests.
