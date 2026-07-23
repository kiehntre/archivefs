# Loose-ROM RetroArch cheat support

ArchiveFS treats supported loose cartridge ROMs as selectable library content,
not archive-mount inputs. This foundation is deliberately limited to Mega
Drive/Genesis files already accepted by the scanner: `.md`, `.gen`, `.smd`, and
contextual `.bin`. Shared identity can also inspect `.sfc` and `.smc` with an
exact SNES platform assignment, but the current scanner does not catalogue
loose SNES files; GUI selection of those files remains deferred rather than
silently broadening library discovery.

## Local-byte identity

A loose-ROM identity request requires an absolute path and exact trusted
scanner/manual platform evidence. The reader rejects traversal components,
symlinked parents, a symlinked final file, non-regular files, unstable
device/inode/size/mtime, unsupported platform-extension combinations, and
files larger than 64 MiB. It opens no-follow on Unix and hashes the entire file
with SHA-256. One file is inspected per request, at most 64 MiB is hashed, 16
warnings and 16 metadata tokens may be retained, and original `PathBuf` bytes
remain the identity for non-UTF-8 paths.

The digest verifies that preview refers to stable local bytes. It does not say
that the ROM is a known-good dump, authentic, safe, legally owned, or compatible
with any emulator. `.smd` is hashed exactly as stored; ArchiveFS does not strip
headers, deinterleave, normalize, rewrite, or calculate a canonical ROM hash.

Trusted-catalogue materialization continues to require exact or already-
approved strong title plus canonical-platform matching. Weak, candidate-only,
ambiguous, excluded, and no-match results remain blocked or distinct. For a
loose ROM the verified local SHA-256 is included in the shared preview identity
and re-read immediately before the GUI invokes apply. Review, confirmation,
separate replacement permission, backup, journal, verification, history, and
rollback behavior are unchanged. Nothing is applied automatically.

## Mount and queue behavior

Loose Mega Drive entries receive `MountState::NotMountable`. Mount, Queue all
visible, Mount All, keyboard/context-menu bulk mounting, and queue execution all
use that shared state plus the archive-kind eligibility check. An old queued
loose entry remains visible and removable but is skipped before a mount attempt;
valid archive entries in the same queue still proceed. Direct core mount
validation independently rejects loose ROMs.

The GUI uses the wording `Loose ROM · no ArchiveFS mount required` and retains
selection, inspection, copy-path, platform, Recently Found, and Cheats & Mods
actions.

## Targeted existing-local inspection

For a selected Mega Drive or SNES title, existing-local RetroArch inspection
looks only at exact reviewed platform directory aliases beneath the selected
profile's configured cheat root, then examines immediate `.cht` children in
deterministic order. It does not recursively walk other platforms and never
modifies a local cheat.

Limits are 16 platform directories, 256 directory entries, 8 MiB per matching
file, and 16 MiB total matching bytes read. Symlinks and unsafe roots are
refused. Results distinguish Not found, normalized filename Candidate, Exact
local filename, Ambiguous, Unsafe, Unavailable, and Limit reached. These labels
describe path evidence only; local cheat compatibility and trust remain
unverified. Trusted ArchiveFS snapshot matching is a separate pipeline.

## Catalogue retrieval bounds

The official immutable catalogue remains bounded at 256 MiB compressed, 1 GiB
expanded, 60,000 extracted entries, 50,000 indexed files, 64 MiB per extracted file, 16
MiB manifest, 1,024 path bytes, 250:1 compression ratio, three redirects, and a
30-second connect, 60-second idle-read, and 15-minute overall transfer bounds.
A failed, cancelled, or timed-out update leaves the prior active
snapshot unchanged and the Sources page states that it remains usable.

## Manual Saltbox/Nobara QA

1. Open Sources and retry the Libretro update.
2. Confirm an immutable archive below 256 MiB can download.
3. Deliberately interrupt networking and confirm the existing snapshot remains active.
4. Select `Alien 3 (USA, Europe).md`.
5. Confirm identity says `Loose ROM`, not `Unsupported platform`.
6. Confirm `MegaDrive` is the exact platform context.
7. Confirm Cheats & Mods reaches match, no-match, ambiguity, candidate, or excluded-entry state rather than identity failure.
8. Confirm an eligible trusted match reaches preview but never applies automatically.
9. Exercise `.sfc`/`.smc` identity with an explicitly supported synthetic SNES context; note that loose SNES scanner discovery is deferred.
10. Confirm loose ROMs show no Mount or Add-to-queue action.
11. Confirm Queue all visible excludes loose ROMs.
12. Confirm a mixed batch still mounts real archives and skips an old loose-ROM queue entry.
13. Confirm existing-local inspection stays within the selected platform subtree and does not reach 10,000 entries.
14. Confirm queue, mounts, Recently Found, filters, History, rollback state, and selection remain intact.

The identity and inspection readers perform local bounded reads only. They do
not mount, extract, write temporary copies, execute ROMs or cheat directives,
start external processes, access the network, upload metadata, or alter emulator
files.
