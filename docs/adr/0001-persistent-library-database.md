# 1. Persistent library database

## Status

Proposed. Design only - no database code has been written yet. See
[`docs/DATABASE_DESIGN.md`](../DATABASE_DESIGN.md) for the full schema, lifecycle, and
integration design this record summarizes.

## Context

ArchiveFS currently has no persistent memory of anything it has scanned.
`ArchiveScanner::scan_archives` walks every configured source folder from scratch on
every single command invocation - `index-build`, `duplicates`, `info`, `mount-one`,
and the GUI's `load_read_only_snapshot` on every launch and every Refresh click. The
closest thing to persistence is `~/.local/share/archivefs/index.json`, which is itself
just a full-rebuild snapshot written by the most recent `index-build`, with no memory
of what changed between one build and the next.

The long-term direction for ArchiveFS includes a persistent library (metadata,
artwork, emulator associations, launch history, favourites, duplicate detection,
library health, mounted-state history, direct launching). All of that needs somewhere
durable to live. Building it directly on top of the JSON index is not workable: JSON
has no query capability, no indexes, and no transactional guarantees, and the current
index is explicitly a disposable, fully-rebuilt cache rather than a record of history.

Two decisions were needed before any of that could be designed in detail: what the
data actually looks like (covered in the design document), and which SQLite crate to
build it on.

## Decision

**Adopt a SQLite-backed catalogue, accessed via `rusqlite` with the `bundled`
feature, once implementation begins.** No dependency is added by this record or by
the design document - this decision applies starting at delivery stage 1 in
`docs/DATABASE_DESIGN.md`.

`rusqlite` was chosen over `sqlx` because it matches this codebase's actual
architecture: the entire workspace is synchronous today (`archivefs-cli`'s `main` is
a plain blocking function; `archivefs-gui`'s `eframe` loop is synchronous
immediate-mode UI), with no async runtime anywhere. `sqlx` is async-first and would
require adopting `tokio` or `async-std` as a new architectural layer purely to run
local SQLite queries - a much larger and unrelated change than "add persistence". The
`bundled` feature statically links SQLite's C source, so no `libsqlite3` system
dependency is needed to build or run ArchiveFS, preserving the same "no extra system
packages required" property already verified for this project's release build (the
release workflow was specifically checked to need zero additional Ubuntu packages,
because `archivefs-gui`'s windowing libraries resolve via runtime `dlopen`, not
link-time linking - a `sqlx`-style system-library dependency would be a step backward
from that).

The second, equally important decision, made throughout the design document rather
than in one place: **the database is never the source of truth for mount safety, and
is not consulted by any mount, unmount, lazy-unmount, or cleanup code path, at any
stage.** Every one of those decisions keeps reading live filesystem state and live
mount state (`mounted_paths_under`, `fs::metadata`, symlink-escape checks) exactly as
it does today. The database is additive: a cache and an observation log over what
`ArchiveScanner` would discover on its own, always safe to delete and rebuild from the
filesystem. This is treated as an architectural boundary, not a convention -
mount/unmount code is not to import from the new `catalogue` module at all, so the
database being missing, empty, or corrupt cannot affect whether ArchiveFS can safely
mount or unmount an archive.

## Consequences

**Positive:**

- Adding local persistence does not force an async runtime onto a codebase that has
  none today; `archivefs-cli` and `archivefs-gui` stay exactly as synchronous as they
  are now.
- `bundled` SQLite keeps the "no extra system packages" property of the existing
  Linux release build intact.
- `rusqlite`'s in-memory database support (`Connection::open_in_memory()`) fits this
  project's existing fast, filesystem-light test style.
- Because mount safety never depends on the database, every stage of the delivery
  plan in the design document can ship independently without risking the one thing
  ArchiveFS cannot regress: safe mounting and unmounting.

**Negative / accepted trade-offs:**

- Giving up `sqlx`'s compile-time-checked queries. Queries against this schema will be
  hand-written and covered by ordinary unit tests instead of macro-verified at compile
  time - judged an acceptable trade for a schema this small, and consistent with this
  project's existing style of a hand-rolled config parser over a general-purpose crate.
- `bundled` SQLite adds real compile time (compiling SQLite's C source via `cc`) to
  every clean build that includes the `catalogue` feature. Accepted as a known,
  one-time cost per clean build rather than a runtime cost.
- No migration framework is adopted; forward-only migrations are plain embedded SQL
  applied in order. This keeps the dependency list smaller but means migration
  ordering/application logic is hand-maintained rather than delegated to a library.

## Alternatives considered

- **`sqlx`** - rejected primarily for forcing an async runtime onto an otherwise fully
  synchronous codebase; its compile-time query checking is a genuine strength but not
  enough to justify that architectural change here.
- **Keep extending the JSON index instead of adding a database** - rejected because
  the goals require indexed search, incremental diffing against history, and
  transactional multi-row updates, none of which a flat JSON file can provide without
  effectively reimplementing a worse version of what SQLite already does.
- **Build the full `docs/database.md` hierarchy (`platforms` -> `titles` -> `releases`
  -> `archives` -> `mounts`/`health_events`) immediately** - rejected for this first
  stage as too large a surface to deliver and test safely at once, and because
  persisting `mount_state` in a `mounts` table (as that document sketches) creates
  exactly the risk this record's mount-safety boundary is meant to prevent: a second,
  potentially stale, place mount state could be read from. `docs/DATABASE_DESIGN.md`
  proposes a smaller schema now and explicitly leaves room to grow toward that
  direction later, once metadata and health-history become real work.
