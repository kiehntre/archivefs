# Cheats & Mods trust, safety, and privacy

ArchiveFS treats users as capable adults while enforcing concrete technical
safety boundaries. The product rule is: **Block dangerous behaviour, not
personal choice.** It inspects, explains, warns, and records where a working
adapter supports those actions; it never secretly executes content or silently
changes an original source.

## Trust and inspection are separate

- **Trusted** means a built-in reviewed source adapter with known provenance,
  format, remote host where relevant, enforced retrieval limits, and available
  integrity information.
- **Unverified** means ArchiveFS has not reviewed the source. Community content,
  local files, personal collections, and content from a friend belong here.
  Unverified does not mean malicious. A future import workflow may use
  structurally valid unverified content after a clear warning and explicit
  consent.
- **Blocked** is reserved for a concrete technical danger or adapter-policy
  violation: unsafe paths, symlinks or special files, malformed structures,
  resource-limit violations, incompatible active content, or content that
  cannot be inspected safely. Missing or uncertain licensing information alone
  is not a technical malware finding.

Inspection has its own state. Passing a structural check does not promote an
unverified source to trusted and does not prove that content is malware-free.
Consent cannot override a concrete technical block.

## What works today

The built-in RetroArch catalogue adapter performs certificate-validated HTTPS
retrieval and local bounded ZIP and catalogue validation. It rejects unsafe
paths, links and special entries, duplicate extraction paths, resource-limit
violations, unsupported structure, and mismatched or corrupt archives. It
records provenance and SHA-256 information in an immutable local snapshot.

Catalogue retrieval alone does not install cheats, modify RetroArch, or copy
files into RetroArch's cheat directory. The first-class GUI workspace currently
provides an in-page archive picker, profile discovery, trusted catalogue
retrieval, and a bounded read-only inventory of the selected profile's existing
RetroArch cheat directory. Archive matching, GUI installation, and all mod
adapters remain unavailable there.

The archive picker searches the already loaded ArchiveFS library by displayed
name, platform, source, mount state, and path. Its tentative selection is
separate from Library focus and mount queues. Applying a choice changes only the
Cheats & Mods context; it never mounts, fetches, installs, or modifies the
archive. Changing away from a context with catalogue retrieval state requires
confirmation.

The existing RetroArch library and ArchiveFS cached catalogue are separate
source modes. Existing-library inspection validates the configured destination
root, refuses symlinked or unsafe paths, and examines at most 10,000 directory
entries to a depth of 16. It counts regular `.cht`-named files without opening
their contents and does not claim compatibility. Missing destinations are not
created. Inaccessible paths and limit exhaustion remain explicit incomplete
results. The inspection itself makes no writes, and the GUI reports that this
workspace has not modified the directory.

There is not yet a general-purpose inspection pipeline for arbitrary local or
community cheat and mod sources. Consequently **Local safety scanning** is
shown as planned/unavailable, no preference is persisted, and the GUI exposes
no toggle that would pretend to change protection.

Before a real default-on setting can ship, the backend needs a safely resolved
local import root, bounded no-follow traversal and archive parsing, adapter
content policies, resource limits, an immutable inspection report, and an
explicit consent hand-off. Disabling it will require confirmation, keep a
visible warning, and never mark uninspected content safe:

> Turning this off does not make unsafe files safe. It only stops ArchiveFS
> checking them.

The confirmation must explain that structural checks stop, protection is
reduced, and no upload occurs in either state. Re-enabling will be a direct
action.

## Unknown code and original files

> ArchiveFS may inspect imported content, but it never executes unknown code
> automatically.

Executables, scripts, installers, macros, and other active content are never
launched during inspection, preview, matching, installation, verification,
rollback, or cleanup. Adapter policy remains format-specific: an executable is
incompatible with a RetroArch cheat catalogue; a Cheat Engine `.ct` document
could be a valid candidate for a future adapter but embedded scripts would
require inspection and would never run automatically; and a Windows mod
installer could be exposed as inspectable high-risk content, but ArchiveFS
would not run it.

ArchiveFS never silently deletes, rewrites, or sanitizes the user's original
source. If sanitized import is implemented, it must create a separate cleaned
copy and an inspectable report listing every excluded item and reason.

## Local privacy

When an implemented adapter performs safety inspection, ArchiveFS scans the
supported imported files locally on your device to help identify unsafe paths,
unexpected executables or scripts, unsupported formats, archive bombs, and
other structural risks. Today this applies to the trusted RetroArch catalogue
retrieval pipeline; it does not describe a general local-import scanner.

Scan results, filenames, file contents, hashes, and metadata are not sent to the
ArchiveFS developers or any third party. ArchiveFS has no telemetry, remote file
submission, cloud malware scanning, developer upload, or third-party analysis
path. This inspection exists solely for the user's protection. Network access
by the trusted catalogue retriever downloads only the reviewed catalogue; it
does not upload local content.

ArchiveFS is not an antivirus scanner. A future unverified-source confirmation
must show the trust and inspection states, detected adapter and formats,
expected/documentation/unexpected files, active and blocked content, warnings,
resource limits, remaining risk, and exactly what ArchiveFS will and will not
do.

## Duplicate and destination safety

No installation control should be exposed until the relevant adapter can check
the emulator-managed library, equivalent files and content digests where
practical, destination conflicts, and whether an item should be reused,
referenced, updated, replaced, or skipped. Existing installers retain their
preview, confirmation, backup, journal, verification, and rollback rules.

## Responsible use

ArchiveFS is intended for preservation, accessibility, personal customization,
and legitimate interoperability. It must not be used to bypass copy protection,
licensing systems, access controls, or other technical protections.

Game developers, artists, musicians, writers, testers, and publishers invest
substantial time and effort in creating games. Supporting legitimate releases
helps make future games, updates, and preservation efforts possible.

You are responsible for ensuring that you have the right to use, modify,
import, and distribute any cheats, patches, mods, textures, or related files.
ArchiveFS does not verify ownership or licensing, does not decide the legality
of individual files, and does not block structurally safe content solely
because licensing information is absent or uncertain.
