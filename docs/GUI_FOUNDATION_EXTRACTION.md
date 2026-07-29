# GUI Foundation Extraction

This milestone adds UI-independent presentation and safety models for the
approved Gamer View / Advanced View redesign. It does not implement either
view's screens, navigation, or actions.

## Modules and responsibilities

### `view_mode`

- `ViewMode::{Gamer, Advanced}` is the stable mode identity.
- `ViewMode::label()` supplies plain display labels.
- `ViewMode::persisted()` and `ViewMode::from_persisted()` provide stable
  lower-case persistence values. The current GUI does not persist its view, so
  this module does not add storage wiring.
- `ViewMode::ALL`, `Display`, and `FromStr` support future controls without
  prescribing their layout.

### `status_wording`

- `StatusContext` accepts existing `MountState` and `IdentityStatus` values,
  plus path and mount applicability facts.
- `plain_status()` returns `PlainStatus { headline, detail }` for Gamer View.
- The mapper does not alter core enums. Technical values stay available to the
  selected-game model for Advanced View.

### `game_presentation`

- `SelectedGamePresentation::from_live()` derives title, platform, format,
  path availability, mount/direct-use applicability, and status from an
  existing `ArchiveRecord`.
- `SelectedGamePresentation::from_cached()` handles a selected cache-only or
  missing `PersistedArchive` without inventing a live mount state.
- `PathAvailability` distinguishes available, known-missing, and unavailable
  paths.
- `GameTechnicalStatus` retains raw archive kind, mount state, identity status,
  and health separately from beginner wording.
- `cheats_mods_available` and `undo_available` are inputs. Claude Code should
  pass the results of the existing adapter/provider/history gates; this module
  deliberately does not reproduce backend capability policy.

### `bulk_confirmation`

- `BulkConfirmation::new(count)` captures the exact item count.
- `mark_preview_complete()` is required before any confirmation can succeed.
- Counts 1 through `TYPED_CONFIRMATION_THRESHOLD` (25) use normal
  confirmation; larger counts require `set_typed_count()` to match exactly.
- `validation()` exposes preview-required, typed-count-required, invalid-count,
  zero-item, cancelled, and confirmed outcomes without executing an action.
- `confirm()` and `cancel()` transition to terminal states. The model owns no
  operation payload and is not wired to Mount All or another real action.

### `selection_guard`

- `SelectionGuard::update()` advances a generation only when exact selection
  changes.
- `SelectionGuard::token()` captures exact selection and generation when async
  work begins.
- `bind_if_current()` rejects a late result unless both fields still match.
- `clear_stale()` clears selection-bound identity or Cheats & Mods presentation
  caches after a selection change.
- `current_value()` prevents a game A value from being read beneath game B.
- These helpers complement the GUI's existing provider request keys and
  generation checks. They do not replace or weaken those protections, and no
  async provider code was rewritten in this milestone.

## Intended integration

The only shared-file integration is five public module declarations near the
top of `crates/archivefs-gui/src/main.rs`. Claude Code can consume the models
incrementally from the new modules while implementing screens. No current
rendering function calls them, so cherry-picking does not change visible
behaviour.

For selected-game presentation, resolve identity availability, Cheats & Mods
availability, and Undo availability through the existing workflow first, then
pass those booleans/status values into `SelectedGamePresentation`. For async
work, retain the existing request key or provider generation and add a
`SelectionToken` at the presentation boundary.

## Conflict risk

Risk against `feature/gui-navigation-reset` is low. New implementation is in
new files. The one likely textual conflict is the small `mod` declaration block
near the imports in `main.rs` if navigation work added modules at the same
location; resolution is to retain both sets of declarations. No screen
composition, navigation enum, `ArchiveFsApp` field, rendering function,
provider, adapter, database, or core enum was edited.

## Cherry-pick order

The commits are intentionally ordered and should be cherry-picked as follows:

1. `f8904d7` — view mode and beginner status wording
2. `332a41a` — selected-game presentation model
3. `d1860d0` — bulk confirmation model
4. `d015df4` — selection generation guards
5. Documentation/validation commit containing this file

The first two are dependent because the selected-game model uses
`status_wording`. The bulk and selection commits are otherwise independent,
but preserving this order gives the tested branch state.

## Deliberately untouched

- Gamer View and Advanced View screen/layout implementation
- navigation, sidebar, gear menu, Settings, Mount, and Cheats & Mods rendering
- action-panel and mode-switch UI wiring
- Mount All and all destructive/backend action wiring
- provider, adapter, identity resolution, and async provider implementations
- archivefs-core enums and backend transaction logic
- databases, migrations, ROMs, and emulator profiles
- broad `main.rs` extraction or unrelated rendering functions
