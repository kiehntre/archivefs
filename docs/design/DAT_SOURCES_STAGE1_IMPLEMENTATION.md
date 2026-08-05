# DAT Sources — Stage 1 Implementation

Status: implemented on `feature/dat-sources-gui-stage1`. This document
describes what shipped, not a proposal. It follows the approved design
references in `docs/design/DAT_CHEAT_POLICY_*.md` (from the design branch)
where they applied to current `main`, and departs from them explicitly where
the codebase they describe had already moved — each departure is called out
below.

---

## 1. Scope

Stage 1 is a read-only DAT source registry and GUI, built entirely on the
existing DAT parsing/indexing/audit core
(`crates/archivefs-core/src/dat/{parser,parsers,index,audit,limits}.rs`). It
adds:

- `crates/archivefs-core/src/dat/sources/` — the registry, its persistence,
  path/format validation, and a read-only audit runner that wires a local ROM
  folder into the existing `audit_files` function.
- `crates/archivefs-gui/src/dat_sources_page.rs` — a GUI page, structured the
  same way as `cheat_sources_page.rs` (a pure view-model function, a
  save/discard draft registry, a background job for long operations).

Nothing in Stage 1 touches DAT parsing, indexing, or audit-verdict logic
itself. It is entirely a registration, persistence, validation-orchestration,
and presentation layer on top of what already existed.

### 1.1 What a user can do

- Register a local DAT file or a folder of DAT files.
- See the detected format and ecosystem, once validated.
- Enable or disable each registered source (registry-only; never deletes
  anything).
- Optionally assign a canonical platform to a source.
- Validate a source (parse it, bounded by `DatLimits`, report per-file
  results).
- Inspect a source's detail: per-file format/version/counts, warnings,
  duplicate catalogue identities, skipped files.
- Run a read-only audit of a chosen folder against a source's catalogue.
- Review audit results, broken down by the core's own verdict categories.
- Remove a source from the registry (never deletes the DAT file, never
  touches ROM data).
- Save or discard all pending edits as one unit.

### 1.2 Departure from the design references

`DAT_CHEAT_POLICY_MODEL.md` and `DAT_CHEAT_POLICY_GUI.md` describe DAT
sources as an extension of the *shared cheat/DAT policy layer* — trust
levels, region/language preferences, revision policy, clone policy,
conflict policy, platform overrides, an effective-policy resolver shared with
cheat sources. None of that shared layer exists on `main`; only the
cheat-source registry (`patch_manager::cheat_source_registry`) does, and it is
`deny_unknown_fields`, so it cannot be extended without breaking every
released build that reads it (see that module's own doc comment).

Stage 1 therefore implements only the parts of the design that current `main`
can actually support without a breaking migration:

| Design concept | Stage 1 status |
| --- | --- |
| Source identity (`SourceId`) | Implemented — `validate_source_id`, same character rules as the model document. |
| Enabled state | Implemented. |
| DAT priority space, default `100` | Implemented as a persisted field. Platform-local ordering is implemented in `sorted_enabled_for_platform`. No priority *editor* is exposed in the GUI (see below). |
| Platform assignment | Implemented as a single optional canonical platform per source (simpler than the model's per-platform `platform_overrides` map — Stage 1 has no per-platform override *of a field*, just "this source's catalogue is this platform's"). |
| Trust level (`Untrusted`/`UserTrusted`/`BuiltInReviewed`) | **Not implemented.** There is no cheat-style trust vocabulary for DAT sources in Stage 1; a source is either registered or not. |
| Region/language preference, revision policy, clone policy, conflict policy, verified-only | **Not implemented.** These are resolution-policy concepts with no consumer yet; DAT audit in Stage 1 always runs the full evidence hierarchy the core already implements. |
| Effective Policy Summary / shared resolver | **Not implemented.** There is no cross-source resolution step in Stage 1; a validate or audit acts on exactly one selected source. |

This is a deliberate scope cut, not an oversight: the model document itself
says extending the shared layer requires "the one-time, explicitly previewed
schema step described in the migration document" before any of §7–§13 can
ship. That schema step is out of scope for Stage 1.

### 1.3 Priority is persisted but not editable in Stage 1

Every registered source gets `priority = 100` by default (matching the
model's default) and the field round-trips through save/load. The GUI does
not expose an editor for it, per the task's own instruction ("do not expose
advanced priority editing unless current architecture makes it necessary for
Stage 1"). With no cross-source audit resolution yet, priority currently has
no observable effect — `sorted_enabled_for_platform` computes the
platform-local ordering the model describes, but nothing in Stage 1 calls it
to pick between two sources during an audit (an audit acts on one selected
source at a time). The field exists so a later stage that does add
multi-source resolution does not need a migration to introduce it.

---

## 2. Supported formats

Exactly the two formats the existing parsers support:

- **Logiqx XML** (`archivefs_core::dat::parsers::logiqx`) — No-Intro, Redump,
  and other Logiqx-shaped DATs.
- **ClrMamePro text** (`archivefs_core::dat::parsers::clrmamepro`) — TOSEC and
  generic ClrMamePro DATs.

Format detection for a *folder* source is stricter than the existing
single-file `detect_format` (`dat/parsers/mod.rs`), which assumes ClrMamePro
for anything it does not recognize — correct for a path a person typed, wrong
for a folder sweep, where the same assumption would silently accept every text
file present. `dat::sources::validation::sniff_dat_format` requires a `.dat`
or `.xml` extension *and* a real Logiqx `<datafile>` root or a `clrmamepro (`
header before a file is added to a folder source's catalogue set; anything
else is reported as skipped, with a reason, never imported silently.

No other format (e.g. MAME XML machine lists using a different root, RomCenter
`.dat`) is claimed as supported.

---

## 3. Persistence

### 3.1 File and format

`~/.config/archivefs/dat_sources.toml`, loaded/saved by
`archivefs_core::dat::sources::config`. Deliberately a **separate file** from
`~/.config/archivefs/cheat_sources.toml`:

- `CheatSourcesConfig` is `#[serde(deny_unknown_fields)]`, so it cannot gain
  a key — including a hypothetical `dat_sources` array — without making the
  file unreadable to every already-released binary.
- `DatSourcesConfig` is the opposite: every unknown key, at both the
  document level and the per-entry level, is captured with
  `#[serde(flatten)] unknown_fields: toml::Table` and re-emitted verbatim on
  save. A future build can add a field and this build will carry it through
  a load/edit/save cycle untouched.

There is no `format_version` field, on the same reasoning the cheat-source
config uses: a version number only matters to a reader that would otherwise
misinterpret the file, and a reader that already preserves what it does not
understand has nothing to misinterpret.

### 3.2 Schema

```toml
# ArchiveFS DAT source registry

[[sources]]
id = "no-intro-nes"
display_name = "No-Intro NES"
path = "/home/user/dats/no-intro-nes.dat"
kind = "file"                    # or "folder"
enabled = true
priority = 100
platform = "NES"                 # optional
origin = "added on the DAT Sources page"   # optional, free text
added_unix_seconds = 1770000000
health_state = "valid"           # not_checked | valid | valid_with_warnings | invalid | unreadable
health_last_validated_unix_seconds = 1770000100
health_detail = "1 entries, 1 ROMs · Logiqx XML"
health_entry_count = 1
health_rom_count = 1
health_formats = ["Logiqx XML"]
health_observed_size_bytes = 512
health_observed_modified_unix_seconds = 1769999000
```

Health fields are flat, prefixed scalars (`health_*`) rather than a nested
`[sources.health]` sub-table, because TOML requires every scalar in a table to
appear before any sub-table in that table, and the entry's own `#[serde(flatten)]`
catch-all can legally contain either shape depending on what a future build
wrote — keeping this build's own fields flat removes that ordering hazard.

### 3.3 Durable writes

`save_dat_sources_config_to` goes through `crate::atomic_write_text`, the
same helper `cheat_sources_config.rs` and `library_views.rs` use: write to a
temp file in the target's own directory, `sync_all`, carry over existing file
permissions, atomically rename over the target, and (on the temp-file error
path) remove the temp file rather than leave it behind. A failed save leaves
the previous file exactly as it was — covered by
`a_failed_save_leaves_the_previous_file_intact` and
`a_save_leaves_no_temporary_file_behind`.

---

## 4. Safety guarantees

### 4.1 Registered-path policy (`dat::sources::validation::validate_dat_path`)

A path typed once on the CLI (`archivefs-cli dat inspect <path>`) is
deliberately exempt from `safe_read`/`TrustedRoots`, per the existing
rationale in `dat/parsers/mod.rs`: the person running the command chose it
directly. A *registered* path is different — it is read again, unattended, on
every later Validate and Audit — so Stage 1 applies a policy at registration
and at every subsequent read:

- must be absolute, with no `.`/`..` component;
- must not be the filesystem root;
- **no component of the path may be a symlink**, and the check walks every
  component, not just the last, so a symlinked *parent directory* is refused
  exactly like a symlinked file;
- must be the kind of thing it was registered as (a file source pointed at a
  directory, or vice versa, is refused with a message telling the user which
  kind to register it as instead).

This is not `TrustedRoots` confinement — DAT files normally live outside any
configured ROM source folder, so confining registration to those folders
would refuse the ordinary case. The symlink rule is what the safety model
requires here, and it is the same rule `safe_read::TrustedRoots::none()`
already enforces elsewhere in the build.

### 4.2 Bounded, safe parsing

Validation calls the existing `parse_dat_file` with `DatLimits::default()`
unchanged — the same file-size, entry-count, ROM-per-entry, identifier-length,
warning-count, and XML-depth ceilings already enforced by the Logiqx and
ClrMamePro parsers. Nothing in Stage 1 loosens or bypasses those limits. A
file above the size ceiling is refused before it is read; a malformed XML
document (unclosed tag, invalid UTF-8) produces a `ParseError` with an
actionable message rather than a panic — verified directly in
`malformed_xml_is_reported_as_invalid_with_an_actionable_error` and
`invalid_utf8_does_not_panic_and_is_reported`.

### 4.3 Folder sources: one level, deterministic, no silent sweep

A folder source scans only its own directory — not subdirectories. This is a
deliberate modeling choice, not merely "unbounded recursion, capped": a DAT
folder is a flat drop of catalogue files, and descending further would sweep
in unrelated trees a user pointed *near*, not *at*. Nested collections are
supported by registering each folder separately.

- Candidates are found by extension (`.dat`/`.xml`) **and confirmed** by
  reading their first ~512 bytes for a real Logiqx or ClrMamePro header — an
  unrelated `.xml` (a config file, notes) is reported as skipped with a
  reason, never silently imported.
- A symlink inside the folder is never followed as a DAT candidate; it is
  reported as skipped, with the reason stated.
- Listing is sorted by filename, so two scans of the same folder produce the
  same order regardless of the filesystem's own directory order.
- Two DAT files that claim the same catalogue identity (header `name` +
  `version`) are both kept and reported as a `DuplicateDatIdentity` — Stage 1
  never guesses which one is "the real one."
- A folder is capped at `MAX_FOLDER_DAT_FILES` (512) files and
  `MAX_FOLDER_ENTRIES_EXAMINED` (20,000) directory entries examined; either
  ceiling produces a `truncated: true` report rather than an unbounded parse.

### 4.4 Audit is read-only

`dat::sources::audit_run::run_dat_audit` is the only place Stage 1 reads ROM
bytes, and it does so exclusively through the existing
`identity_source::hashing::hash_file_reporting`, which itself opens files only
through `safe_read::open_bounded_read`. The only filesystem calls anywhere in
`audit_run.rs` are `read_dir`, `symlink_metadata`, and that hashing call —
there is no `create`, `write`, `rename`, `remove`, `set_permissions`, or
symlink operation anywhere in the module. `an_audit_makes_no_change_to_the_files_it_reads`
and its GUI-level counterpart `an_audit_changes_nothing_on_disk` snapshot the
entire scanned tree byte-for-byte before and after a run and assert equality.

The folder walk is:

- bounded to `MAX_SCAN_DEPTH` (8) directories and `MAX_SCAN_FILES` (25,000)
  files, reporting `truncated: true` rather than silently covering only part
  of a library while claiming completeness;
- breadth-first over an explicit queue (not recursion), so depth is a number
  the function enforces rather than a property of the call stack;
- does not follow a **symlinked directory** (which could cycle); a symlinked
  *file* is collected and left to the same read policy every other hash in
  the build goes through, so it is followed only when the caller supplies
  `TrustedRoots` that cover it — never by default.

A file the hashing policy refuses (a broken symlink outside trusted roots, a
file above the automatic-hash size ceiling, etc.) is still audited **by name
only**, and is listed separately in `DatAuditOutcome::unhashed` with the
refusal reason — so a `FilenameOnly` verdict for such a file is visibly
distinguished from a verdict backed by an actual hash comparison.

Cancellation: a caller-supplied `AtomicBool` is checked before every DAT file
read, before every scanned file, and inside every hash chunk (the same
per-chunk check `hash_file_reporting` already implements), so cancelling a
run over a multi-gigabyte file takes effect within one chunk. The GUI runs
both Validate and Audit on a background thread and polls a bounded
(`sync_channel`, depth 64) progress channel every frame; a full channel drops
the oldest progress message rather than blocking the worker, so the audit
itself never waits on the UI thread.

### 4.5 Removal never deletes anything but the registry entry

`DatSourceRegistry::remove` and the GUI's `Remove` action operate only on the
in-memory (then, on Save, persisted) registry list. No path is touched.
Covered directly by `removing_a_source_removes_the_registry_entry_and_nothing_else`
(core) and `removing_a_source_never_deletes_the_dat_file` (GUI), both of which
snapshot the file/folder before and after removal and assert byte-for-byte
equality.

---

## 5. Audit result meanings

The GUI presents exactly the eight verdict categories
`archivefs_core::dat::audit::AuditVerdict` already defines — none merged, none
invented:

| Category | Meaning |
| --- | --- |
| Exact | A cryptographic hash (SHA-256, SHA-1, or MD5) matched exactly one catalogue entry. |
| Exact (multiple) | A cryptographic hash matched several catalogue entries; all are listed. |
| Probable | CRC32 (with size, where known) matched one entry. Weaker evidence than a cryptographic hash. |
| Probable (multiple) | CRC32 matched several entries. Deliberately not "Exact": a 32-bit checksum collision is as plausible as a genuine duplicate. |
| Filename only | The name is in the catalogue and no hash was available. Says a *name* matched, not that this file did. |
| Ambiguous | Candidates exist but the evidence disagrees (e.g. CRC32 matches an entry whose declared size does not). |
| Not in catalogue | Hashes were compared and matched nothing. |
| No usable evidence | No hash could be compared, and the filename matched nothing either. |

Provenance travels with every result: source ID, display name, the DAT
path(s) actually read, the catalogue header name(s), and the scanned folder
are all part of `DatAuditOutcome`, so a result can be attributed long after
the underlying source's state has moved on. If the source that produced a
result is later removed from the registry, the GUI drops the result rather
than continuing to show it attributed to something no longer there.

---

## 6. Deferred (out of scope for Stage 1)

Per the task's explicit deferral list, and consistent with
`DAT_CHEAT_POLICY_MODEL.md §12` (`RenameSafety::NeverSuggest` is the only
implemented value in this codebase, full stop):

- Automatic rename of ROM files to match catalogue names.
- Automatic move of ROM files.
- Deletion of ROM files or DAT files.
- Archive rewriting.
- Symlink rewriting.
- Automatic repair of any kind.
- Downloading DATs from any online provider.
- Any network access whatsoever (Stage 1 adds none — see §7).
- Automatic priority resolution / cross-source conflict resolution during an
  audit (an audit acts on one explicitly selected source).
- Bulk correction actions.
- The full shared DAT/cheat policy layer described in
  `DAT_CHEAT_POLICY_MODEL.md` (trust levels, region/language preference,
  revision policy, clone policy, conflict policy, verified-only, the
  Effective Policy Summary, and the shared resolver) — deferred as a
  follow-up stage, since it requires a schema-migration step this stage does
  not attempt.
- A priority *editor* in the GUI (the field is persisted; see §1.3).

---

## 7. Compatibility

- **Cheat Sources is unchanged.** No file under
  `patch_manager/cheat_source_registry/` or `cheat_sources_page.rs` was
  modified.
- **Gamer View covers and platform artwork are unchanged.** No file under
  `gamer_artwork.rs`, `platform_artwork.rs`, or their supporting modules was
  touched.
- **Existing config continues to load.** `Config::load_default()` and
  `CheatSourcesConfig` are untouched; a clean install with no
  `dat_sources.toml` renders an empty DAT Sources page with no error (see
  `a_fresh_install_renders_an_empty_page_with_no_error`).
- **No migration exists to damage older config,** because there is no older
  format for this file — Stage 1 is the first version, so there is nothing to
  migrate from.
- **No network access is added.** Every new module's only I/O is local
  filesystem reads and one local file write (the registry itself);
  `crates/archivefs-core/Cargo.toml`'s existing `ureq`/`http`/`url`
  dependencies are used by pre-existing code (RomM, cheat provider fetches)
  and are not invoked anywhere in `dat::sources`.
- **Viewing DAT Sources performs no ROM writes.** Opening the page only reads
  the registry file (lazily, on first visit, exactly as Cheat Sources does);
  Validate and Audit read DAT/ROM files; only Save writes, and it writes only
  `dat_sources.toml`.
