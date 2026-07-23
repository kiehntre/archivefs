# RetroArch controlled apply, history, and rollback

ArchiveFS can connect an eligible RetroArch trusted-catalogue match to the
shared transaction engine. This is a narrow cheat-file workflow, not a general
mod installer. PCSX2 and Dolphin remain preview-only.

## Materialized source and eligibility

The source is the original regular file inside an already-local immutable
catalogue snapshot. ArchiveFS neither downloads nor copies it during preview or
apply. The snapshot directory name, source ID, manifest binding, freshness,
complete validation, catalogue parse, exact path containment, manifest entry,
file digest, selected archive, platform, profile, source mode, destination, and
match strength are checked. Symlinks, lossy paths, changed or missing files,
stale/incomplete snapshots, ambiguous matches, weak/candidate matches, and
paths outside the snapshot fail closed.

Exact and existing core-approved strong matches are eligible. The bridge
materializes at most 64 entries. The shared preview and transaction limits then
apply unchanged; see `SHARED_CHEAT_PREVIEW.md` and
`SHARED_SAFE_APPLY_ROLLBACK.md` for byte, entry, journal, backup, path, warning,
and locking bounds.

## GUI flow

Cheats & Mods displays the immutable snapshot ID/root, exact source and digest,
destination state, proposed action, blockers, backup requirement, and separate
replacement-permission requirement. An eligible RetroArch write follows:

1. Preview
2. Review the exact plan ID, source, destination, and digest
3. Confirm the operation; separately opt in to replacement when required
4. Apply through the shared locked transaction engine
5. Verify the resulting digest and journal result
6. Display success, partial success, or failure per entry

Replacement permission is never preselected. Candidate, weak, ambiguous,
conflicting, stale, PCSX2, and Dolphin previews expose no Apply control.
Double submission is prevented by the operation state, and core confirmation is
bound to the exact plan. A changed archive, adapter, profile, source mode,
source, destination, page context, or platform invalidates review state; core
revalidation remains authoritative immediately before writing.

## History and rollback

History & Logs loads the existing shared journals in the background, bounded by
the shared history reader, and orders valid operations newest-first. It shows
the operation and plan IDs, timestamp, adapter, selected archive, source mode,
destination, entry outcomes, verification, backup state, overall status, and
rollback state. An apply result opens its exact operation. Malformed or
unsupported journals are summarized as retained warnings and do not hide valid
operations or crash the page. Existing session activity remains separate.

Rollback always starts with a fresh read-only preview. The user sees current
destination and backup state and can confirm only the exact preview ID. The
shared rollback engine refuses changed user content, missing/changed backups,
unsafe paths, root mismatch, and already-completed rollback. Install-new
rollback removes only the still-matching journal-owned file; replacement
rollback atomically restores and verifies the retained backup. History is
refreshed after completion.

## Safety, privacy, and manual Nobara QA

Preview, history loading, and rollback preview are local and read-only. Writes
occur only after exact confirmation through the shared transaction pipeline.
There is no network retrieval during preview/apply/rollback, upload, telemetry,
process execution, emulator launch, silent overwrite, or conflict
auto-resolution. Catalogue originals are never modified; unrelated files are
never targeted; verified replacement backups are retained.

Manual Nobara checks:

1. Select a synthetic archive whose platform and title exactly match a fresh,
   locally cached trusted snapshot entry and an eligible RetroArch profile.
2. Confirm the preview shows the snapshot ID, exact source digest, and expected
   destination; verify PCSX2 and Dolphin show no Apply control.
3. Review an install-new plan, cancel once, and verify no destination, backup,
   or journal was created; then confirm and verify the final result and journal.
4. For different existing content, verify the confirmation is disabled until
   the separate replacement approval is selected and that the backup remains.
5. Open the exact operation in History & Logs, refresh it, and verify malformed
   synthetic journals appear only as aggregate warnings.
6. Preview and confirm rollback; verify modified destinations or changed
   backups block rollback and a second rollback is unavailable.
7. Change archive/profile/source/page during preview or review and verify the
   stale result cannot be applied. Confirm queue, mounts, Library selection, and
   platform assignment are unchanged throughout.
