# Manual QA plan - ArchiveFS v0.5.0-alpha

This is a manual, check-box acceptance plan for the v0.5.0-alpha release. It
complements automated tests; it does not replace them. Run
`cargo test --workspace` and the rest of
[`docs/release-checklist.md`](release-checklist.md) separately.

## How to use this plan

Each test lists an **action**, an **expected result**, and a blank
**failure notes** line to fill in if the actual result differs. Every
action is labelled with where it happens:

- **On Saltbox** - a step performed on the remote machine/server hosting
  your archive source folders (verifying files exist, checking network
  storage is reachable, or inspecting anything from outside the desktop
  session).
- **On Nobara desktop** - a step performed on the Linux desktop machine
  outside the ArchiveFS window itself (a terminal command, resizing the
  OS window, restarting the app, checking `~/.config/archivefs`).
- **In ArchiveFS GUI** - a step performed by clicking or typing inside the
  running `archivefs-gui` window.

If your own setup doesn't have a separate "Saltbox"-style machine, treat
"On Saltbox" steps as "wherever your archive source folders actually live"
and adapt paths accordingly - the expected result is what matters, not the
exact machine name.

Sections 15-19 cover the Cheats & Mods workspace's three adapters -
RetroArch, PCSX2, and Dolphin - grouped together. **Historical note:**
Section 20 (Dolphin) was originally written as conditional on a
not-yet-merged branch; Dolphin merged before the `v0.5.0-alpha` tag was
cut, so this section is unconditional for any build from that tag onward.
See [`docs/MANUAL_QA_v0.6.0-alpha.md`](MANUAL_QA_v0.6.0-alpha.md) for the
current consolidated plan.

Test with a small, disposable, non-important set of archives. Do not run
this plan against your only copy of anything you cannot afford to lose,
even though every action described here is documented as non-destructive.

---

## 1. First launch

- [ ] **Action (On Saltbox):** Confirm at least 2-3 small test archives
      (`.zip`/`.7z`/`.rar`) exist in a source folder reachable from the
      desktop machine, and that folder is mounted/reachable.
  - Expected: files are visible from the Nobara desktop at the configured
    source path.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Confirm `ratarmount` is installed and
      on `PATH` (`ratarmount --version`).
  - Expected: a version string prints; no "command not found".
  - Failure notes:
- [ ] **Action (On Nobara desktop):** With no existing
      `~/.config/archivefs/config.toml`, launch `archivefs-gui`.
  - Expected: the app opens a window (no crash), and offers a path to
    create a starter config rather than failing silently.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Edit `~/.config/archivefs/config.toml`
      to point `source_folders` at your test archive folder and
      `mount_root` at a writable test directory, then relaunch
      `archivefs-gui`.
  - Expected: the app loads without error and reaches the Library page.
  - Failure notes:

## 2. Navigation

- [ ] **Action (In ArchiveFS GUI):** Look at the left-hand page list.
  - Expected: `Mount`, `Selected`, `Active Mounts`, `Library`, `Sources`,
    `Cheats & Mods`, `Doctor`, `History & Logs`, `Settings`, `About` are
    all present as primary destinations, with `Health`, `Duplicates`, and
    `Library Views` reachable as a secondary group below them.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click each destination once in turn.
  - Expected: each page loads without a crash or a stuck spinner; the
    clicked destination is visibly highlighted as the active page.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** From any page, use the menu bar (File
      / Library / Sources / Tools / Help) where present.
  - Expected: menu items that navigate to a page land on that same page
    reachable from the left-hand list; nothing duplicates or conflicts.
  - Failure notes:

## 3. Mount

- [ ] **Action (In ArchiveFS GUI):** Open **Mount** with at least one
      archive that is not yet mounted.
  - Expected: the archive appears with a planned destination path and a
    "Ready to mount"-style status; nothing has been mounted yet.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Queue" (or equivalent) on one
      archive, then look at the queue count.
  - Expected: the queue count increases by one; the archive is visibly
    marked as queued; no mount has happened yet.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Clear queue".
  - Expected: the queue empties; no archive is mounted as a result.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Queue an archive, then click
      "Mount queue"/"Mount now" and confirm.
  - Expected: an explicit confirmation step appears before anything
    mounts; after confirming, a result summary shows attempted/successful/
    failed/skipped counts.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** After the mount above reports
      success, check the mount point in a terminal (e.g. `mount | grep
      archivefs` or `ls` the configured `mount_root`).
  - Expected: the mount point exists and is readable, matching what the
    GUI reported.
  - Failure notes:

## 4. Selected

- [ ] **Action (In ArchiveFS GUI):** With an empty queue, open **Selected**.
  - Expected: a clear "no archives queued" empty state with a path back to
    Mount, not a blank or broken page.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Queue 2+ archives on Mount, then open
      **Selected**.
  - Expected: every queued archive appears with its planned destination
    and planned action; nothing has been mounted yet.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Remove one item from the Selected
      queue.
  - Expected: only that item disappears from the queue; the rest remain
    in their original order.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** From Selected, confirm and mount the
      remaining queue.
  - Expected: the same confirmation and result-summary behavior as the
    Mount page; results match what was actually queued.
  - Failure notes:

## 5. Active Mounts

- [ ] **Action (In ArchiveFS GUI):** With nothing mounted, open
      **Active Mounts**.
  - Expected: a clear "no archives are currently mounted" empty state.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Mount an archive (via Mount or
      Selected), then open **Active Mounts**.
  - Expected: the mounted archive appears with its destination and an
    "Unmount" action.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Unmount" and confirm.
  - Expected: an explicit confirmation appears before unmounting; after
    confirming, the archive disappears from Active Mounts.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** After the unmount above, check the
      mount point in a terminal.
  - Expected: the mount point is gone/empty, matching the GUI.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Refresh" on Active Mounts after
      mounting/unmounting outside the current view.
  - Expected: the list updates to the true current mount state.
  - Failure notes:

## 6. Library

- [ ] **Action (In ArchiveFS GUI):** Open **Library** with your test
      archives scanned in.
  - Expected: every test archive is listed with a platform, state, archive
    path, and mount path column.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Type into the search/filter field.
  - Expected: the visible rows narrow to matches only; clearing the filter
    restores the full list.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click a row, then Ctrl+click a second
      row.
  - Expected: both rows show as selected; a bulk-action bar appears where
    applicable.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a single archive and click
      "Inspect contents" (only enabled for a supported format).
  - Expected: a read-only listing of the archive's internal entries opens;
    nothing is extracted to disk.
  - Failure notes:

## 7. Context menus

- [ ] **Action (In ArchiveFS GUI):** Right-click a single Library row.
  - Expected: a context menu opens with Mount/Unmount, Inspect contents,
    Copy archive/mount/source path, and (when eligible) "RetroArch
    Cheats"; the row's own selection state does not change just from
    right-clicking.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select multiple rows, then right-click
      one of them.
  - Expected: the multi-selection is preserved (right-clicking does not
    collapse it to a single row), and bulk-appropriate actions (Mount
    selected / Unmount selected) appear.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Right-click a row whose archive path
      contains unusual characters, if you have one, or an archive on a
      filesystem with non-UTF-8 names.
  - Expected: the menu opens without a crash; the path displays (lossily
    if necessary) rather than panicking.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Right-click inside a text field
      (e.g. the search box) rather than a table row.
  - Expected: the normal text-editing context menu (Copy/Cut/Paste/Select
    all) appears and is unaffected by the row context-menu changes in
    this release.
  - Failure notes:

## 8. Sources

- [ ] **Action (In ArchiveFS GUI):** Open **Sources**.
  - Expected: your configured source folder(s) are listed with status,
    archive counts, and last-scan information.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Scan" on one source.
  - Expected: a background scan runs (the app stays responsive); the
    source's counts/status update afterward.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Scan all enabled" with 2+
      sources configured.
  - Expected: all enabled sources are rescanned; a per-source summary is
    shown.
  - Failure notes:
- [ ] **Action (On Saltbox):** Add a new small test archive file to the
      scanned source folder.
  - Expected: the file is present and readable from the desktop machine
    before the next scan.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Rescan the source from the step above.
  - Expected: the newly added archive appears in Library afterward.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Add folder" and cancel without
      choosing a folder.
  - Expected: no source is added; existing sources are unaffected.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Remove a test source using
      "keep records" mode, then using "remove records" mode (on a
      throwaway test source, not your real library).
  - Expected: "keep records" leaves catalogue rows in place; "remove
    records" removes them; neither mode touches the actual files on disk.
  - Failure notes:

## 9. Doctor

- [ ] **Action (In ArchiveFS GUI):** Open **Doctor**.
  - Expected: a "Healthy" or "Needs attention" badge, a one-line check
    summary, and the full list of named checks with pass/warn/fail status.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Run all checks".
  - Expected: the checks re-run (button disabled while busy) and the
    summary/badge update to match the new results.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Copy summary", then paste
      somewhere (e.g. a text editor).
  - Expected: a plain-text report with the config path, summary, and every
    check's status/name/detail is pasted.
  - Failure notes:

## 10. History export

- [ ] **Action (In ArchiveFS GUI):** Perform a few operations (a mount, an
      unmount, a source scan) so History & Logs has entries.
  - Expected: each operation appears as an entry with a timestamp, action,
    and outcome.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Open **History & Logs** and use the
      Operation and Result filters.
  - Expected: the visible entries narrow to match the selected filter(s);
    "Clear filters" restores the full list; the shown/total count updates
    correctly.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Toggle "Sort: Newest/Oldest First".
  - Expected: entry order reverses without gaining, losing, or duplicating
    any entry.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Copy visible log", then paste.
  - Expected: exactly the currently-filtered entries are copied as text.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Export log", choose a save
      location in the native file dialog, and confirm.
  - Expected: the dialog opens (this is a synchronous native dialog - the
    app may briefly wait on it, which is expected) and a text file is
    written to the chosen location containing the visible entries.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Open the exported file from a
      terminal or text editor.
  - Expected: its contents match what "Copy visible log" produced for the
    same filter state.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Clear history".
  - Expected: entries are removed from the panel; this does not affect any
    already-exported file or any mounted/unmounted archive state.
  - Failure notes:

## 11. Settings

- [ ] **Action (In ArchiveFS GUI):** Open **Settings**.
  - Expected: configuration path, database path, and mount root are shown
    read-only, each with a working "Copy" action.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Validate configuration".
  - Expected: a configuration check runs and its pass/fail state is shown.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Open configuration folder".
  - Expected: your desktop's file manager opens at
    `~/.config/archivefs` (or the app reports why it could not, if no
    file manager is available).
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Copy environment report", then
      paste.
  - Expected: version, OS/architecture, desktop environment, database
    schema version, and paths are included as plain text; any field
    ArchiveFS doesn't know is reported as "unknown", not omitted or
    guessed.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Look for any density/appearance
      toggle or "Reset all settings" control.
  - Expected: none is present, or if shown, it is clearly labelled as not
    yet functional - Settings must not offer a control that silently does
    nothing.
  - Failure notes:

## 12. Health

- [ ] **Action (In ArchiveFS GUI):** With no catalogue scanned yet
      (fresh config), check whether **Health** is reachable from the
      navigation list.
  - Expected: Health is visibly disabled with an explanation, not silently
    missing, until a scan has produced data.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** After scanning, open **Health**.
  - Expected: a dashboard of health issues (if any) is shown, matching
    what Doctor/Library report for the same archives.
  - Failure notes:

## 13. Library Views

- [ ] **Action (In ArchiveFS GUI):** Open **Library Views** with none
      configured.
  - Expected: a clear empty state, not an error.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Create a new view (e.g. grouped by
      platform), then preview it.
  - Expected: the preview shows planned symlink actions without creating
    anything until you apply it.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Apply the view, then check the
      target directory.
  - Expected: symlinks are created there; your original archive files are
    untouched.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Inspect the applied view's target
      directory in a terminal.
  - Expected: entries are symlinks pointing at the real archive paths, not
    copies.
  - Failure notes:

## 14. Cheats & Mods

- [ ] **Action (In ArchiveFS GUI):** Open **Cheats & Mods** with no
      archive selected.
  - Expected: a "Choose one archive" empty state with a path to Library;
    no archive context is fabricated.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select one archive in Library, then
      open **Cheats & Mods**.
  - Expected: the page shows that exact archive as the current context
    (name, platform/source), plus status badges indicating trusted
    catalogue retrieval is available while matching and installation are
    not.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Choose another archive" from
      Cheats & Mods.
  - Expected: you're returned to Library to make a new selection; nothing
    is installed or changed as a side effect of navigating.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Scroll to the "Stage 3 - Matching and
      installation" and "Mods" sections.
  - Expected: both are clearly labelled "Not available" / "Planned" with
    explanatory text, not disabled-but-implied-working controls.
  - Failure notes:

## 15. RetroArch adapter - profile discovery

- [ ] **Action (In ArchiveFS GUI):** In Cheats & Mods, with profiles not
      yet scanned, look at Stage 1.
  - Expected: a clear "not scanned yet" state with a "Scan for RetroArch
      profiles" action; no profile is assumed.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Scan for RetroArch profiles".
  - Expected: the scan runs in the background (the UI stays responsive)
    and completes with discovered profiles or "no eligible profile found".
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** If exactly one eligible profile is
      found, check whether it is pre-selected.
  - Expected: it is - ArchiveFS may pre-select the single eligible option,
    but must never silently choose between two or more eligible profiles.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** If a profile is ineligible/blocked,
      check its detail.
  - Expected: a concrete blocker code and explanation is shown, not just
    "unavailable".
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Click "Rescan profiles".
  - Expected: discovery re-runs and results update; a stale/previous
    selection that is no longer valid is cleared rather than silently
    kept.
  - Failure notes:

## 16. RetroArch adapter - trusted catalogue fetch

- [ ] **Action (In ArchiveFS GUI):** With an eligible profile chosen, open
      Stage 2 and click "List trusted sources".
  - Expected: only the fixed, built-in trusted source list appears - there
    is no field to type a custom URL anywhere on this page.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a trusted source, then click
      "Fetch / update catalogue".
  - Expected: the fetch runs in the background (UI stays responsive); on
    success, a status, digest, retrieval time, and freshness are shown.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Temporarily disable network access
      (e.g. disconnect Wi-Fi/Ethernet), then retry "Fetch / update
      catalogue" In ArchiveFS GUI.
  - Expected: a truthful failure message is shown (not a hang, not a
    silent success); the app remains usable.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Re-enable network access before
      continuing.
  - Expected: subsequent fetches succeed again.
  - Failure notes:

## 17. RetroArch adapter - cached reuse

- [ ] **Action (In ArchiveFS GUI):** After a successful fetch above, click
      "Use cached snapshot".
  - Expected: the cached snapshot loads immediately (no network wait) and
    is clearly marked as reused/offline rather than freshly fetched.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Disable network access, then repeat
      "Use cached snapshot" In ArchiveFS GUI.
  - Expected: the previously cached snapshot is still usable entirely
    offline.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Re-enable network access.
  - Expected: normal operation resumes.
  - Failure notes:

## 18. RetroArch adapter - force refresh

- [ ] **Action (In ArchiveFS GUI):** With a fresh cached snapshot already
      present, check "Force a full refresh instead of reusing a fresh
      cache", then click "Fetch / update catalogue".
  - Expected: a real network fetch happens even though a fresh cached
    snapshot already existed, and the result reflects a fresh download
    rather than a silent cache reuse.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Uncheck force refresh and fetch again
      immediately.
  - Expected: the now-fresh cache is reused instead of a new download.
  - Failure notes:

## 19. PCSX2 adapter

- [ ] **Action (On Nobara desktop):** Confirm a PCSX2 installation (native
      or Flatpak) is present on the test machine, with at least one
      `cheats` or `cheats_ws` directory existing under its configuration
      root.
  - Expected: PCSX2's configuration directory and at least one patch
    subdirectory exist and are readable.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Prepare a safe, disposable `.pnach`
      fixture (or use an existing read-only one you don't mind ArchiveFS
      reading) in that `cheats` directory - do not use anything
      irreplaceable.
  - Expected: the fixture file is present and readable before continuing.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a PlayStation 2 archive in
      Library, then open **Cheats & Mods**.
  - Expected: PCSX2 appears as the default adapter for this archive;
    RetroArch remains separately selectable.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a non-PS2 archive (e.g. a
      RetroArch-platform title) and open **Cheats & Mods**.
  - Expected: PCSX2 does **not** appear for this archive - it is
    PS2-only.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** With the PS2 archive selected, inspect
      the listed PCSX2 profiles.
  - Expected: eligible profiles are shown distinctly from blocked ones,
    each blocked profile carrying a concrete, typed reason rather than a
    vague "unavailable"; if exactly one profile is eligible it is
    pre-selected, and if more than one is eligible you must choose
    explicitly.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Inspect the selected profile's
      `cheats` and `cheats_ws` path reporting.
  - Expected: both configured paths are shown; a missing directory is
    reported as missing/normal, not as an error, and is not created by
    ArchiveFS as a side effect of viewing it.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Inspect the PNACH inventory for the
      fixture file prepared above.
  - Expected: the fixture appears with its filename-derived CRC/serial
    candidate, category, size, and any warnings; nothing about the file
    is modified by inspecting it.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** After the inspection above, check the
      fixture file's modification time and contents in a terminal.
  - Expected: unchanged from before ArchiveFS inspected it.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Read the match-state wording shown for
      the fixture (exact / ambiguous / no-match / unavailable).
  - Expected: the wording never claims an "exact match" unless a verified
    PS2 executable CRC was supplied - in ArchiveFS's current state that
    should not happen, so an exact-match claim here would itself be a
    defect to report.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Look across the entire PCSX2 section
      for any Install, Apply, Enable, Disable, Delete, Replace, Fix, or
      rollback control.
  - Expected: none exists. Every action available is inspection-only
    (viewing, scanning, choosing a profile, choosing an archive).
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** With the PCSX2 inventory loaded for
      archive A, switch to a different PS2 archive B in Library, then
      switch back to A.
  - Expected: B's inventory replaces A's cleanly; switching back to A
    re-scans or restores A's own data - a stale result from A must never
    be shown labelled as B's, or vice versa.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Before, during, and after the PCSX2
      inspection above, check the Mount queue, Active Mounts, and the
      selected archive's platform assignment in Library.
  - Expected: none of these change as a side effect of opening Cheats &
    Mods, choosing a PCSX2 profile, or inspecting PNACH files.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Resize the window to laptop
      (~1280px), ~1536x864, and ultrawide widths with the PCSX2 section
      open.
  - Expected: the PCSX2 profile/PNACH content reflows the same way the
    rest of Cheats & Mods does at each width, with no clipped or
    inaccessible control.
  - Failure notes:

## 20. Dolphin adapter (only after `codex-dolphin-readonly-adapter` is merged)

**Do not run this section against a build from `sonnet-v0.5-release-prep`
as it stands today.** The Dolphin read-only adapter described here has
been implemented and validated on a separate branch,
`codex-dolphin-readonly-adapter`, and has **not** been merged into this
branch. Run this section only once that merge has happened and you are
testing a build that actually contains it - otherwise "Dolphin does not
appear" is expected, not a defect. Even once merged, note that Codex's own
validation ran on Ubuntu 24.04.4 LTS, not Nobara - a genuine Nobara run is
still owed and this section is the place to record it.

- [ ] **Action (On Saltbox):** Build and prepare the merged branch/tag for
      deployment to the Nobara test machine; no Dolphin-specific action is
      needed on Saltbox beyond normal build/deploy.
  - Expected: a build containing the merged Dolphin adapter is available
    on the Nobara desktop.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Launch the new build.
  - Expected: the app starts normally with no new crash or hang introduced
    by the Dolphin code.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Confirm a Dolphin installation
      (native or Flatpak) is present on the test machine, with a
      root-level `Dolphin.ini` at its configuration root.
  - Expected: Dolphin's configuration directory and root-level
    `Dolphin.ini` exist and are readable. If the displayed configuration
    path anywhere in ArchiveFS shows `Config/Dolphin.ini` instead of the
    real root-level file, that is a defect to report.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Prepare a safe, disposable
      `GameSettings/<id>.ini` fixture (lowercase `.ini`) containing an
      `[ActionReplay]` or `[Gecko]` section for one game folder under that
      Dolphin profile - do not use anything irreplaceable.
  - Expected: the fixture file is present and readable before continuing.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a GameCube or Wii archive in
      Library, then open **Cheats & Mods**.
  - Expected: Dolphin appears as the default adapter for this archive;
    RetroArch and PCSX2 remain separately selectable and do not appear as
    the default.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a non-GameCube/Wii archive
      (e.g. a PS2 or RetroArch-platform title) and open **Cheats & Mods**.
  - Expected: Dolphin does **not** appear for this archive - it is
    GameCube/Wii-only.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** With the GameCube/Wii archive
      selected, inspect the listed Dolphin profiles.
  - Expected: eligible profiles are shown distinctly from blocked ones,
    each blocked profile carrying a concrete, typed reason; if exactly one
    profile is eligible it is pre-selected, and if more than one is
    eligible you must choose explicitly.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Select a Dolphin profile for a game
      that has no `GameSettings` directory at all.
  - Expected: the profile remains eligible - a missing `GameSettings`
    directory is reported as normal, not as a blocker, and nothing is
    created by ArchiveFS as a side effect of viewing it.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Inspect the Game INI inventory for the
      fixture prepared above.
  - Expected: the fixture appears with its filename-derived Game ID
    candidate, category, size, and any warnings; nothing about the file
    is modified by inspecting it.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** After the inspection above, check the
      fixture file's modification time and contents in a terminal.
  - Expected: unchanged from before ArchiveFS inspected it.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Confirm your Dolphin data root's
      texture, `GraphicMods`, `ResourcePacks`, and Riivolution asset
      directories exist (if you have any), then check whether ArchiveFS
      represents them anywhere in the Dolphin section.
  - Expected: they are **not** represented anywhere - no texture pack,
    graphics mod, resource pack, or Riivolution asset is inspected or
    listed. Any such content appearing would be a defect to report.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Read the match-state wording shown for
      the fixture (exact / ambiguous / no-match / unavailable).
  - Expected: the wording never claims an "exact match" unless the shared
    identity section shows a verified Dolphin Game ID read from supported
    disc metadata. A filename candidate alone must never produce that claim.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Look across the entire Dolphin section
      for any Install, Apply, Enable, Disable, Delete, Replace, Fix, or
      rollback control.
  - Expected: none exists. Every action available is inspection-only
    (viewing, scanning, choosing a profile, choosing an archive).
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** With the Dolphin inventory loaded for
      archive A, switch to a different GameCube/Wii archive B in Library,
      then switch back to A.
  - Expected: B's inventory replaces A's cleanly; switching back to A
    re-scans or restores A's own data - a stale result from A must never
    be shown labelled as B's, or vice versa.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Before, during, and after the Dolphin
      inspection above, check the Mount queue, Active Mounts, and the
      selected archive's platform assignment in Library.
  - Expected: none of these change as a side effect of opening Cheats &
    Mods, choosing a Dolphin profile, or inspecting Game INI files.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Resize the window to laptop
      (~1280px), ~1536x864, and ultrawide widths with the Dolphin section
      open.
  - Expected: the Dolphin profile/Game INI content reflows the same way
    the rest of Cheats & Mods does at each width, with no clipped or
    inaccessible control.
  - Failure notes:

### Shared verified game identity (synthetic fixtures only)

- [ ] **Action (On Nobara desktop):** Add synthetic, non-game ISO and ZIP
      fixtures representing PS2, GameCube, and Wii to an isolated test source,
      select each in Cheats & Mods, and observe the shared identity section.
  - Expected: inspection runs without freezing the UI; archive, platform,
    evidence states, source method, diagnostics, read counts, and exact adapter
    match state remain visible. No raw binary data is rendered.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** While a synthetic identity read is in
      progress, change archive, adapter, page, and platform context in turn.
  - Expected: every superseded result is rejected and never appears under the
    new context.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Observe the synthetic fixtures and their
      parent directory before and after inspection, and monitor running
      processes and network activity.
  - Expected: no file, timestamp, directory, mount, emulator process, helper
    process, or network connection is created or changed by identity
    inspection.
  - Failure notes:

### Shared read-only preview (synthetic fixtures only)

- [ ] **Action (On Nobara desktop):** Use isolated synthetic PCSX2 and Dolphin
      sources with verified synthetic identities and destinations that are
      missing, identical, different, symlinks, directories, and special files.
  - Expected: Shared Preview reports the exact source and destination, typed
    destination state, proposed action, eligibility, backup requirement, and
    replacement-permission requirement. It always states "Preview only. No
    files were changed." and exposes no mutation controls.
  - Failure notes:

- [ ] **Action (In ArchiveFS GUI):** During preview, change archive, adapter,
      profile, source mode, destination, page, and platform context one at a
      time.
  - Expected: each superseded result is discarded; it never appears under the
    new context, and the mount queue, current mounts, and selected platform are
    unchanged.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Compare the isolated fixture tree before
      and after missing/identical/different/conflict previews while monitoring
      processes and network activity.
  - Expected: no file or directory is created, written, renamed, deleted,
    mounted, or timestamp-modified intentionally; no process is launched and no
    network connection is made. Only bounded regular-file reads occur.
  - Failure notes:

### Shared safe apply and rollback (synthetic fixtures only)

- [ ] **Action (On Nobara desktop):** In an isolated temporary tree, create a
      synthetic trusted RetroArch `.cht` source and exact eligible preview.
      Review the six stages, cancel once, then explicitly confirm install-new.
  - Expected: cancellation writes nothing. Confirmation is bound to the exact
    plan; one destination appears atomically, verifies, and has one journal.
    No emulator, helper process, mount, or network connection is started.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Preview replacement of a synthetic
      different `.cht`; confirm generally without replacement permission, then
      repeat with the separate replacement choice enabled.
  - Expected: the first attempt preserves the original. The second creates and
    verifies an operation-scoped backup before atomic replacement, retains the
    backup, and records exact hashes and paths in History.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Change archive, adapter, profile, source
      mode, page, or platform after review and before confirmation.
  - Expected: the stale plan cannot apply. Queue, mount, selected archive, and
    platform state remain unchanged.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Preview rollback, modify the installed
      destination, and attempt confirmation; restore the expected installed
      bytes, preview again, confirm, and then attempt rollback a second time.
  - Expected: modified content blocks rollback. The fresh exact preview safely
    removes an install-new file or restores a verified replacement backup. The
    second rollback is unavailable and unrelated content remains untouched.
  - Failure notes:

## 21. No-archive state

- [ ] **Action (In ArchiveFS GUI):** Clear the Library selection entirely
      (e.g. "Clear selection"), then open **Selected** and **Cheats &
      Mods**.
  - Expected: both pages show a calm "no archive selected" empty state
    with a way back to Library, not an error or blank screen.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** With zero archives scanned at all
      (fresh source folder), open **Library**, **Mount**, and
      **Active Mounts**.
  - Expected: each shows an appropriate empty state describing what to do
    next (e.g. "scan a source"), not a crash.
  - Failure notes:

## 22. Archive context preservation

- [ ] **Action (In ArchiveFS GUI):** Select an archive in Library, open
      Cheats & Mods, scan profiles and list trusted sources, then
      navigate to another page (e.g. Doctor) and back to Cheats & Mods.
  - Expected: the same archive context and Stage 1/2 state are still
    present - nothing resets just from navigating away and back.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** With the Cheats & Mods workflow open
      for archive A, go to Library and select a different archive B.
  - Expected: returning to Cheats & Mods shows archive B's context, not a
    stale mix of A and B; the workflow does not silently keep A's data
    under B's name, regardless of which adapter (RetroArch, PCSX2, or
    Dolphin) applies to each archive.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Right-click a Library row and choose
      "RetroArch Cheats" for a different archive than the one currently
      open in Cheats & Mods.
  - Expected: the workflow correctly switches to the newly chosen archive.
  - Failure notes:

## 23. Activity collapsed/expanded state

- [ ] **Action (In ArchiveFS GUI):** Launch the app fresh and look at the
      Activity panel at the bottom.
  - Expected: it starts **collapsed** (compact), not taking up a large
    portion of the window by default.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Perform an operation that fails (for
      example, try to mount an archive whose source file was moved/
      deleted), with Activity collapsed.
  - Expected: the collapsed summary makes the failure noticeable rather
    than only showing a bare count.
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Expand Activity, then collapse it
      again.
  - Expected: toggling works from both the panel itself and, if present,
    the Tools menu equivalent, without desyncing.
  - Failure notes:

## 24. Safe mount

- [ ] **Action (In ArchiveFS GUI):** Mount a test archive and confirm.
  - Expected: only the destination shown in the preview is created;
    nothing outside the configured `mount_root` is touched.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Check file permissions/ownership of
      the mounted content.
  - Expected: the mount is read-only; attempting to write into it (e.g.
    `touch <mount_path>/testfile`) fails.
  - Failure notes:
- [ ] **Action (On Saltbox):** Confirm the original source archive file is
      unchanged (size/modification time) after mounting and unmounting.
  - Expected: no change to the original file.
  - Failure notes:

## 25. Inspect mounted contents

- [ ] **Action (In ArchiveFS GUI):** With an archive mounted, select it in
      Library and click "Inspect contents".
  - Expected: internal entries are listed read-only; nothing is extracted
    to a persistent location.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Browse the actual mount point in a
      file manager or terminal while the inspector is open.
  - Expected: the files visible on disk match what the inspector lists.
  - Failure notes:

## 26. Normal unmount

- [ ] **Action (In ArchiveFS GUI):** Unmount a mounted archive from
      Active Mounts (see section 5) or from Library's selected-archive
      panel.
  - Expected: an explicit confirmation is required; after confirming, the
    archive shows as unmounted everywhere in the app (Library, Active
    Mounts, Mount).
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** Attempt to unmount while a terminal
      or file manager still has the mount point open as its current
      directory (On Nobara desktop, `cd` into the mount point in another
      terminal first).
  - Expected: ArchiveFS reports a truthful failure/recovery guidance
    (e.g. suggesting the file manager/terminal be closed) rather than
    silently forcing anything; Lazy Unmount, if offered, is a distinct,
    clearly-labelled recovery step, not the default action.
  - Failure notes:

## 27. Restart persistence where applicable

- [ ] **Action (On Nobara desktop):** Close `archivefs-gui` entirely and
      relaunch it.
  - Expected: source folders, library views, and cached RetroArch
    trusted-catalogue snapshots persist across the restart; the History &
    Logs panel is empty again (this is documented as in-memory/
    per-session in this release, not a bug).
  - Failure notes:
- [ ] **Action (In ArchiveFS GUI):** After restarting, confirm previously
      mounted archives are shown with their true current mount state
      (not assumed still-mounted).
  - Expected: mount state reflects the real filesystem, refreshed on
    launch/refresh, not a stale assumption from before the restart.
  - Failure notes:

## 28. Window resizing

Test at three widths: a small laptop width (~1280px), the reference
~1536x864 size, and an ultrawide width (~3440px or your widest available
display/window).

- [ ] **Action (On Nobara desktop):** Resize the ArchiveFS window to a
      small laptop width (~1280px wide) In ArchiveFS GUI's own window,
      then check Library, Mount, and Cheats & Mods.
  - Expected: content reflows to a compact layout; tables scroll
    horizontally rather than clipping unreadably; no control becomes
    inaccessible.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Resize the window to approximately
      1536x864.
  - Expected: this is the primary reference size; layout should look
    balanced with no excessive empty gutters or cramped controls.
  - Failure notes:
- [ ] **Action (On Nobara desktop):** Resize the window to an ultrawide
      width (or maximize on an ultrawide display).
  - Expected: page content uses a bounded maximum width rather than
    stretching every control across the full screen; wide tables (e.g.
    Library, Mount) may use more width than prose pages (e.g. Settings,
    About).
  - Failure notes:
- [ ] **Action (On Nobara desktop):** At each width above, confirm the
      left-hand navigation list remains usable and every page destination
      stays clickable.
  - Expected: navigation does not break, overlap, or get clipped at any
    tested width.
  - Failure notes:

---

## Sign-off

- [ ] Sections 1-19 and 21-28 completed.
- [ ] Section 20 (Dolphin) completed. **Historical note:** at the time
      this plan was written, Dolphin's merge into this branch was still
      pending; it merged before the `v0.5.0-alpha` tag was cut, so this
      section is unconditional for any build from that tag onward. See
      [`docs/MANUAL_QA_v0.6.0-alpha.md`](MANUAL_QA_v0.6.0-alpha.md) for the
      current consolidated plan.
- [ ] Every failure note either resolved or explicitly deferred with a
      linked issue/tracking note.
- [ ] `docs/release-checklist.md`'s Code section re-run and green on the
      exact commit being considered for tagging.
