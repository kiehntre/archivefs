# Manual QA plan - ArchiveFS v0.6.0-alpha

This is the consolidated manual, check-box acceptance plan for the
`v0.6.0-alpha` release, covering everything merged since `v0.5.0-alpha`:
shared verified game identity, shared preview/conflict detection, the
shared safe apply/backup/journal/rollback foundation, RetroArch GUI
apply/history/rollback, RetroArch trusted-catalogue download and
management, Recently Found, and Mega Drive loose-ROM recognition. It
complements automated tests; it does not replace them. Run
`cargo test --workspace` and the rest of
[`docs/release-checklist.md`](release-checklist.md) separately.

This document supersedes the Cheats & Mods-specific portions of
[`docs/MANUAL_QA_v0.5.0-alpha.md`](MANUAL_QA_v0.5.0-alpha.md) for the new
RetroArch apply/catalogue-manager workflows; the v0.5 plan's sections on
Mount, Selected, Active Mounts, Library, Sources, Doctor, Settings, Health,
Library Views, and window resizing remain valid and are not repeated here
except where they interact with the new work.

## How to use this plan

Each test lists an **action**, an **expected result**, and a blank
**failure notes** line. Actions are labelled:

- **On Saltbox** - a step on the remote machine/server hosting your
  archive source folders.
- **On Nobara desktop** - a step outside the ArchiveFS window itself.
- **In ArchiveFS GUI** - a step inside the running `archivefs-gui` window.

**Before you start:** the RetroArch trusted-catalogue parse-tolerance
issue described in `docs/V0.6_RELEASE_AUDIT.md` and `ROADMAP.md`'s
"Current development" section is **still open** as of this plan. Section
7's checks below are expected to reveal it if it's present in the build
under test - do not treat a failure there as a new regression until you've
checked whether it's this known, already-tracked issue.

Test with a small, disposable, non-important set of archives.

---

## 1. First launch

- [ ] **Action (On Nobara desktop):** Launch `archivefs-gui` on a clean
      install with no prior config.
  - Expected: starter-config flow works as before; no crash.
  - Failure notes:

## 2. Settings scrolling

- [ ] **Action (In ArchiveFS GUI):** Open Settings, expand Activity, and
      scroll to the final control with the mouse wheel, then Page Down,
      then Home/End.
  - Expected: all keys work; the final control is reachable at every
    window size.
  - Failure notes:

## 3. Doctor and History scrolling

- [ ] **Action (In ArchiveFS GUI):** Repeat the same scroll-key check on
      Doctor and on History & Logs.
  - Expected: identical behavior to Settings (shared scrollable-page
    wrapper).
  - Failure notes:

## 4. Library scan

- [ ] **Action (In ArchiveFS GUI):** Scan a source folder containing a mix
      of `.zip`/`.7z`/`.rar` archives.
  - Expected: scan completes; summary counts match what was added.
  - Failure notes:

## 5. Recently Found

- [ ] **Action (In ArchiveFS GUI):** After the scan above, open Recently
      Found.
  - Expected: shows only the newest scan's `added` archives, in path
    order; restart the app and confirm the view persists.
  - Failure notes:

## 6. Mega Drive `.md` detection

- [ ] **Action (On Nobara desktop):** Place `Alien 3 (USA, Europe).md`
      under an exactly-named `megadrive` (or `genesis`) source folder, and
      an unrelated `README.md` outside it.
  - Expected after rescan: the ROM is catalogued as platform `MegaDrive`
    and appears in Recently Found; the unrelated `README.md` is skipped as
    ambiguous, not imported.
  - Failure notes:

## 7. `README.md` rejection

- [ ] **Action (In ArchiveFS GUI):** Confirm the unrelated `README.md`
      from step 6 never appears in Library or Recently Found.
  - Expected: rejected/ignored, never catalogued as an archive.
  - Failure notes:

## 8. RetroArch catalogue Download

- [ ] **Action (In ArchiveFS GUI):** Open Sources with no cached
      catalogue; click Download.
  - Expected: a review dialog names the provider and the exact
    ArchiveFS-managed destination before any network access; confirm, and
    the retrieval completes with an exact 40-character revision, file
    count, and verification state shown.
  - Failure notes:

## 9. RetroArch catalogue Update

- [ ] **Action (In ArchiveFS GUI):** With a cached catalogue already
      present, click Update.
  - Expected: same review-then-confirm flow; on success the active
    snapshot's revision/timestamp updates.
  - Failure notes:

## 10. RetroArch catalogue Verify

- [ ] **Action (In ArchiveFS GUI):** Click Verify on an existing snapshot.
  - Expected: read-only, no network access, reports the current
    verification state.
  - Failure notes:

## 11. Retained active snapshot after failed update

- [ ] **Action (On Nobara desktop):** Disconnect networking, then click
      Update **In ArchiveFS GUI**.
  - Expected: the failure is reported truthfully (typed, not a hang or
    silent success), and the previously active snapshot remains active
    and usable.
  - Failure notes:

## 12. Exact match

- [ ] **Action (In ArchiveFS GUI):** Select an archive with a known exact
      RetroArch catalogue match.
  - Expected: preview shows an exact match with real provenance (snapshot
    ID, source digest); no fabricated confidence.
  - Failure notes:

## 13. No match

- [ ] **Action (In ArchiveFS GUI):** Select an archive with no catalogue
      entry.
  - Expected: preview truthfully reports "No matching cheat found," no
    Apply control.
  - Failure notes:

## 14. Candidate/ambiguous blocking

- [ ] **Action (In ArchiveFS GUI):** Select an archive whose only matches
      are weak, candidate-only, or ambiguous.
  - Expected: no Apply control is offered for any of these; the state is
    shown distinctly from an exact match, not silently upgraded.
  - Failure notes:

## 15. Install new

- [ ] **Action (In ArchiveFS GUI):** Apply an exact match to a destination
      with nothing there yet.
  - Expected: no backup created (none needed); confirmation required
    before any write; journal records `InstalledNew`.
  - Failure notes:

## 16. Already installed

- [ ] **Action (In ArchiveFS GUI):** Repeat step 15 for an identical
      already-present file.
  - Expected: preview reports "Already installed"; no silent rewrite.
  - Failure notes:

## 17. Replacement with backup

- [ ] **Action (In ArchiveFS GUI):** Apply over an existing *different*
      file.
  - Expected: a separate, non-preselected replacement-approval checkbox is
    required before Apply is enabled; a verified backup is created before
    the write; the backup persists after success.
  - Failure notes:

## 18. History deep link

- [ ] **Action (In ArchiveFS GUI):** From History & Logs, open the exact
      operation from step 15 or 17.
  - Expected: shows the real plan ID, adapter, destination, and outcome.
  - Failure notes:

## 19. Rollback preview

- [ ] **Action (In ArchiveFS GUI):** Preview a rollback of the step-17
      replacement.
  - Expected: shows current destination and backup state before any
    action is taken.
  - Failure notes:

## 20. Rollback success

- [ ] **Action (In ArchiveFS GUI):** Confirm the rollback from step 19.
  - Expected: the original file is restored and verified; the backup
    remains available afterward.
  - Failure notes:

## 21. Rollback blocked after user modification

- [ ] **Action (On Nobara desktop):** Manually edit the installed
      destination file outside ArchiveFS, then attempt rollback **In
      ArchiveFS GUI**.
  - Expected: rollback is blocked with a clear reason; nothing is silently
    overridden.
  - Failure notes:

## 22. Restart persistence

- [ ] **Action (On Nobara desktop):** Close and relaunch the app.
  - Expected: Recently Found, the RetroArch catalogue cache, and History &
    Logs journal entries survive the restart (History & Logs' in-session
    activity view is expected to reset - that is documented, not a bug).
  - Failure notes:

## 23. Queue and mount preservation

- [ ] **Action (In ArchiveFS GUI):** With an active mount queue and at
      least one mounted archive, run through steps 8-20 in full.
  - Expected: queue contents, mount state, Library selection, and platform
    assignment are all unchanged at the end.
  - Failure notes:

## 24. No network during Apply

- [ ] **Action (On Nobara desktop):** Monitor network activity (e.g.
      `nethogs`/`ss`) during steps 15-20.
  - Expected: zero network activity during preview, apply, or rollback -
    only the explicit Download/Update steps (8-9) should ever touch the
    network.
  - Failure notes:

## 25. No process execution

- [ ] **Action (On Nobara desktop):** Monitor process execution (e.g.
      `strace -f -e trace=execve`) during steps 12-21.
  - Expected: zero process spawns from any Cheats & Mods RetroArch
    workflow.
  - Failure notes:

## 26. PCSX2 no Apply

- [ ] **Action (In ArchiveFS GUI):** Select a PS2 archive with an eligible
      PCSX2 profile and inspect its PNACH preview.
  - Expected: no Install/Apply/Enable/Disable/Replace/Rollback control
    exists anywhere in the PCSX2 section - inspection only.
  - Failure notes:

## 27. Dolphin no Apply

- [ ] **Action (In ArchiveFS GUI):** Select a GameCube/Wii archive with an
      eligible Dolphin profile and inspect its Game INI preview.
  - Expected: same as PCSX2 - no apply control of any kind, inspection
    only.
  - Failure notes:

---

## Must-pass release checks

Sections 1, 4-27 above.

## Nice-to-have checks

- Section 2-3's scrolling checks repeated at laptop/1536x864/ultrawide
  window widths.
- Network/process monitoring (sections 24-25) repeated across the full
  RetroArch apply/rollback cycle, not just a single pass.

## Unavailable checks due to missing real fixtures

- A genuine crash-mid-write test (killing the process during an actual
  apply) has no synthetic fixture in the current test suite; this needs a
  deliberately engineered manual exercise, not a routine QA pass.
- Testing against the real, full-size official RetroArch catalogue at
  scale (as opposed to a small synthetic or single-fetch test) is
  possible today via step 8 above, but a large-scale tolerance stress test
  against many thousands of real catalogue entries should wait until the
  parse-tolerance issue in Section "Before you start" is fixed, since a
  failure there is currently expected rather than diagnostic.

## Sign-off

- [ ] Sections 1, 4-27 completed.
- [ ] Sections 2-3 completed or explicitly deferred with a tracking note.
- [ ] Every failure note either resolved or explicitly deferred with a
      linked issue/tracking note.
- [ ] The RetroArch catalogue parse-tolerance issue's actual status
      (still open / fixed and merged) is recorded here, not assumed:
      _______________________
- [ ] `docs/release-checklist.md`'s Code section re-run and green on the
      exact commit being considered for tagging.
