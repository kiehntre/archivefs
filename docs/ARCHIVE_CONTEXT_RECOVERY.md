# Archive context and GUI recovery

## State invariants

`ArchiveContext` is the sole owner of primary archive identity in the GUI.

- `focused` drives Library details, Selected, and the active Cheats & Mods
  archive.
- `selected` is the exact-path multi-selection used by Library highlighting
  and bulk actions.
- Cheats & Mods derives its active archive from `focused`; its workflow path
  is only a binding key for cached asynchronous results.
- `mount_queue` is independent of focus and selection.
- mounted archives are derived from the current live records. Mounting,
  unmounting, and refreshing mount state do not clear archive context.
- rescans prune a focused/selected path only when it no longer exists in the
  merged live and persistent catalogue.
- replacing the focused archive replaces the whole adapter workflow, dropping
  stale candidates, selections, previews, workers, and transaction state.

## Adapter routing

Routing is derived from the active archive's canonical platform and is not a
user-selectable tab:

- GameCube and Wii route only to Dolphin.
- PS2 routes only to PCSX2.
- other known platforms route to RetroArch.
- absent or `Unknown` platforms render an unsupported state.

The RetroArch catalogue manager is rendered only on a RetroArch route.

## Library layout

The Library table owns the remaining page height. The old persisted summary
and selected-archive `TopBottomPanel` heights were removed because their
combined 964-pixel maximum could starve the nested table scrollers. Focused
details are collapsed and bounded; secondary filters are collapsed; the
activity panel has a viewport-relative maximum. The horizontal table scroller
and virtualized vertical rows remain independent and usable.

## Dolphin manual proof

1. Open Library and search for `Animal Crossing`.
2. Select `/mnt/games/roms/gcn/Animal Crossing (USA).zip`.
3. Open Selected and confirm the same exact path is shown.
4. Open Cheats & Mods. Confirm the route is Dolphin, platform is GameCube,
   game ID is `GAFE01`, and revision is `0`.
5. If no eligible profile is found, enter
   `/home/davedap/Applications/Dolphin/User` as the additional Dolphin
   directory.
6. Select **Rescan Dolphin profiles**. Confirm the profile configuration path
   and its `GameSettings` directory are displayed.
7. Select **Fetch Gecko codes**. Confirm the external Dolphin upstream provider
   reports exact game ID `GAFE01`, USA, revision `0`, its source URL and
   attribution, and the `16:9 Widescreen` candidate. This must work even when
   `GameSettings/GAFE01.ini` does not exist.
8. Select one code and choose **Preview the installed file**. Review the exact
   destination, selected names, complete generated sections, preserved
   sections, creation/backup state, warnings, and SHA-256.
9. Review and confirm apply. Open Dolphin and verify the chosen Gecko code is
   displayed for Animal Crossing.
10. Return to ArchiveFS and use **Roll back this install**; verify Dolphin's
    prior file is restored exactly, or that a file created by this transaction
    is removed.

ArchiveFS never invents Gecko definitions. GAFE01 definitions come from the
official Dolphin upstream GameSettings dataset. Provider retrieval, apply,
Dolphin display, and rollback still require manual proof on the real profile;
automated tests use recorded provider fixtures and isolated temporary paths.
