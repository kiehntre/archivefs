# Managed Library Views

A Library View is a named, symlink-based organized view of the catalogue.
It lets you browse archives grouped a particular way (currently:
`{platform}/{filename}`) in a separate directory tree, without moving,
copying, or extracting the underlying archives, and without changing your
`source_folders`.

This is a preview/organization feature, not a mount backend: the symlinks a
view creates point at your source archive files, the same way a normal
symlink would. Mounting archives for read access remains a separate,
explicit operation (`mount`, `mount-one`, or the GUI mount actions).

## Why it exists

Archives discovered from `source_folders` may be laid out however you
originally organized them - by download date, by source, by nothing in
particular. A Library View lets you additionally see the same archives
organized by platform (today's only layout template) in one destination
directory, for browsing or for pointing another tool at a predictable
structure, while your original folders stay untouched.

## Configuration

Views are stored at `~/.config/archivefs/library_views.json`, one entry per
view:

- `id` - a stable identifier generated once (`generate_library_view_id`)
  and never reused, independent of `name`. Manifests are keyed by `id`, so
  renaming a view never orphans its manifest.
- `name` - a human-readable label.
- `destination_root` - where the view's managed symlinks are created. It
  must not be inside any configured source folder, and no source folder may
  be inside it; this is validated the same way source-folder overlap is
  validated.
- `enabled` - whether the view is currently active.
- `source_folders` - which configured source folders to include; empty
  means all of them.
- `platforms` - which platforms to include; empty means every known
  (non-`Unknown`) platform. Archives with an `Unknown` platform are always
  skipped, regardless of this setting.
- `layout_template` - currently only `PlatformFilename` (`{platform}/{filename}`)
  is supported. This is a fixed enum, not a free-form string template, so an
  invalid layout can never be configured.

## Commands

```sh
archivefs view list
archivefs view preview <name-or-id>
archivefs view apply <name-or-id>
archivefs view repair <name-or-id>
archivefs view remove <name-or-id> [--keep-definition]
```

- `list` prints configured views.
- `preview` computes and prints the current plan for a view - what would be
  created, repaired, removed, or skipped - without touching the filesystem.
  Safe to run as often as you like.
- `apply` creates and repairs a view's managed symlinks according to the
  current plan.
- `repair` is the same operation as `apply`: it both creates anything
  missing and fixes drift (for example a symlink whose target has changed).
- `remove` removes a view's managed symlinks. By default it also removes the
  view's own configuration entry; `--keep-definition` removes only the
  symlinks and keeps the view configured so it can be applied again later.

All four `view` subcommands accept `--json` for machine-readable output.

## What a plan can say

Planning classifies every catalogue entry against the view's current
manifest (its record of exactly which symlinks it previously created) into
one of:

- **Create** - the symlink doesn't exist yet and would be created.
- **AlreadyCorrect** - the symlink exists and already points at the right
  target.
- **Repair** - the symlink exists but is wrong (for example stale, or
  pointing at a moved archive) and would be corrected.
- **RemoveStale** - a previously created symlink no longer corresponds to a
  current catalogue entry and would be removed.
- **Collision** - two archives would map to the same destination path, or
  the destination path is already occupied by something the view did not
  create. Apply never overwrites a collision.
- **SkipUnknownPlatform** / **SkipMissingSourceArchive** / **SkipInvalidPath** -
  the entry is intentionally left alone, with the specific reason recorded.

If the view's `destination_root` itself is unsafe (inside a source folder,
containing one, or unresolvable), planning sets `unsafe_root_error` and
`apply`/`repair` refuse to run, no matter how clean the individual entries
look.

## Safety properties

- Views only ever create, repair, or remove symlinks that the view's own
  manifest recorded as belonging to it. `repair` and `remove` never touch a
  path the view didn't create itself.
- Planning performs no filesystem mutation - it only reads existing
  symlink/file state to classify what's already there.
- A view never mounts or unmounts anything, and never touches a source
  archive file.
- Manifests are plain JSON, one per view, under
  `~/.local/share/archivefs/library_views/{id}.manifest.json`. A missing
  manifest is treated as "this view has never been applied yet," not an
  error, exactly like a missing top-level config.
