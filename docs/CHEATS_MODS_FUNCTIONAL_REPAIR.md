# Cheats & Mods functional repair (`sonnet-cheats-mods-functional-repair` branch)

This document covers the future mod-adapter work `show_mods_section`'s
compact notice points at, kept separate from the functional-repair
milestone itself per that milestone's explicit instruction not to build a
complete mods system in this pass.

## Current state

`show_mods_section` (main.rs) renders a single compact `widgets::banner`,
not a full section with its own card:

- PCSX2: read-only PNACH inventory is already available elsewhere on the
  page (the PCSX2 workflow's own profile/inventory cards); this banner
  states plainly that preview, installation, enabling, disabling,
  replacement, and rollback are not available for it yet.
- Dolphin: same shape, for its own frame-patch/Action Replay/Gecko/
  Riivolution inventory.
- Any other adapter (including RetroArch, and no archive selected): a
  "Planned" banner using the existing `MODS_UNAVAILABLE_BODY` copy - no
  mod workflow exists yet for any system.

No fake actions (Install/Browse/Download/Apply/Remove) are ever offered -
covered by `mods_section_has_no_fake_user_actions`.

## What a real mod-adapter workflow would need

Not attempted in this milestone; listed here so a future milestone has a
concrete starting point rather than reinventing the survey:

1. **A verified mod catalogue**, analogous to the RetroArch cheat
   catalogue's trusted-source list - PCSX2 widescreen/texture patches and
   Dolphin's Action Replay/Gecko/Riivolution content have no equivalent
   `CheatSourceList`/manifest/digest-verification machinery today.
2. **A shared preview/apply story** for mods, mirroring
   `build_shared_preview`/`materialize_retroarch_shared_preview`/
   `execute_shared_apply` (the exact reused pipeline the functional
   repair milestone wired up for RetroArch cheats) - mods would need
   their own materialization step (or reuse of the same one) plus
   destination-path rules per adapter.
3. **Per-adapter installation semantics** - PCSX2 patch directories and
   Dolphin's GameSettings INI structure are read differently from
   RetroArch's `.cht` cheat files; the existing PCSX2/Dolphin inspection
   code (`inspect_pcsx2_profile`, `inspect_dolphin_profile`) is read-only
   and does not yet know how to write.
4. **Enable/disable/replace/rollback semantics** - explicitly called out
   as unavailable in both read-only banners today; these are the exact
   operations Library Views' apply/rollback and the RetroArch cheat
   install pipeline already model, so the shared journal/backup
   machinery (`SharedApplyOptions`, `SharedApplyConfirmation`,
   `execute_shared_apply`, `execute_shared_rollback`) is the right
   foundation to extend rather than a new one to invent.

None of the above was implemented or scaffolded in this milestone - this
is a survey of what exists to reuse, not a design commitment.
