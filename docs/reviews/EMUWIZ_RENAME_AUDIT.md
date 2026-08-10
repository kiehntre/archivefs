# EmuWiz rename audit

Audit date: 2026-08-10.

This audit covers the documentation and user-facing branding rename. EmuWiz
was previously known as ArchiveFS. The repository, Cargo package names,
configuration and data locations, environment variables, schemas, and
serialized identifiers are deliberately outside that rename; the executable
names use a staged rename with legacy aliases (below).

## Classification

Every case-insensitive occurrence of the old name after the rename fits one of
the following three categories. No missed user-facing rename is known.

### 1. Internal or compatibility identifiers

| Retained identifier or family | Why it remains |
|---|---|
| `archivefs-core`, `archivefs-cli`, `archivefs-gui`, `archivefs_core`, and paths under `crates/archivefs-*` | Cargo package/crate names, Rust import paths, and source-tree paths are stable internal interfaces. |
| `ArchiveFsApp`, `ArchiveFsError`, and `ArchiveFsTrustedCatalogue` | Existing Rust type and enum-variant names are internal code identifiers. |
| `archivefs_path`, `archivefs_prefix`, `archivefs_root`, `archivefs_platform_id`, `archivefs_platform_display_name`, and `archivefs_writes_here` | Existing fields and model vocabulary include persisted or serialized identifiers and are not branding strings. |
| `--archivefs-root` (RomM identity mappings) | Existing CLI flag name; scripts that pass it keep working. |
| `archivefs_folder_refresh`, `archivefs_game_records_examined`, `archivefs_duplicate_search`, `archivefs_health_search`, `archivefs_inspector_search`, `archivefs_library_search_filter`, `archivefs_will_do`, and `archivefs_will_not_do` | Internal widget IDs, counters, and test inspection names must remain stable and are not displayed as the product name. |
| `ARCHIVEFS_LOG` | Legacy environment variable, still honoured during the compatibility period; `EMUWIZ_LOG` now wins when both are set (see the environment-variable section). |
| `ARCHIVEFS_LOCK_CHILD`, `ARCHIVEFS_LOCK_ROOT`, `ARCHIVEFS_LOCK_HOLD_MILLIS`, `ARCHIVEFS_TEST_HOT_JOURNAL_DB`, and `ARCHIVEFS_TEST_HOT_JOURNAL_MARKER` | Test subprocess protocols, not user-facing. |
| `ARCHIVEFS_PCSX2_PROOF` | A documented shell variable used by the existing proof procedure; environment-variable names are outside the rename. |
| `~/.config/archivefs`, `~/.local/share/archivefs`, `/mnt/archivefs`, `/var/lib/archivefs`, and example equivalents | Existing configuration, data, mount, fixture, and test paths must continue to resolve without migration. See the path-compatibility section. |
| `archivefs-v*`, `archivefs-*` temporary names, release payload members, test-fixture names, and script defaults | Artifact and fixture naming follows the unchanged package/repository compatibility surface. |
| `archivefs/<version>` HTTP `User-Agent` | Existing outbound client identifier; changing remote-facing protocol identity is deliberately deferred from this compatibility baseline. |
| `kiehntre/archivefs`, repository-relative links containing `archivefs`, and checkout/worktree examples | The GitHub repository is intentionally not renamed, so its URLs and checkout directory remain valid. |
| `// ArchiveFS managed block: <id>` and `// End ArchiveFS managed block` | These PCSX2 delimiters are parsed ownership markers already written into user files. Changing them would break recognition, migration, diagnostics, and rollback. Tests and design documentation retain the exact bytes. |
| `[ArchiveFS_Managed_GameHacking]` | This Dolphin INI section is an existing ownership marker written into user files. Readers, writers, diagnostics, tests, and documentation retain the exact section name. |
| Test names such as `a_pnach_with_an_archivefs_marker_and_no_install_record_is_reported`, `removal_only_touches_archivefs_managed_entries`, `robots_disallows_archivefs`, and related variants | These are internal Rust test identifiers describing compatibility behavior; they are not user-visible product copy. |

Bare lowercase `archivefs` occurrences are also retained when they are components
of package metadata, lockfiles, paths, URLs, filenames, command examples,
temporary-directory prefixes, test data, or internal search fixtures. They do
not present the old name as the current product brand.

### 2. Historical references

The exact old product name remains where it describes the earlier project or a
versioned snapshot:

- the migration sentence in `README.md` and this audit;
- `CHANGELOG.md` entries describing changes made under the earlier name;
- `docs/MANUAL_QA_v0.5.0-alpha.md` and
  `docs/MANUAL_QA_v0.6.0-alpha.md`;
- `docs/RELEASE_NOTES_v0.5.0-alpha.md` and
  `docs/RELEASE_NOTES_v0.6.0-alpha.md`;
- versioned records under `docs/releases/`;
- `docs/V0.6_RELEASE_AUDIT.md` and `docs/V0.7_RELEASE_HARDENING.md`;
- the pre-rename snapshot audits already under `docs/reviews/`.

Current documents, current design documents, help text, GUI and CLI strings,
product-descriptive comments, examples, and corresponding test expectations use
EmuWiz.

### 3. Missed user-facing rename

None found after the repository-wide case-insensitive audit.

## Binary naming strategy (staged)

The Cargo packages remain `archivefs-core`, `archivefs-cli` and `archivefs-gui`
- renaming crates and their import paths is deferred to a later, separate
migration. The emitted executables use a staged rename with compatibility
aliases:

| Package | Emitted binaries |
|---|---|
| `archivefs-cli` | `emuwiz-cli` (primary), `archivefs-cli` (legacy alias) |
| `archivefs-gui` | `emuwiz` (primary), `emuwiz-gui` (alias), `archivefs-gui` (legacy alias) |

CLI help, version and examples present `emuwiz-cli` as the command name. The
installer installs `emuwiz-cli` and `emuwiz` and creates `emuwiz-gui`,
`archivefs-cli` and `archivefs-gui` symlinks, so both GUI spellings and existing
scripts keep working. Release bundles ship the two primary binaries only.

## Configuration and data path compatibility

Directory resolution is EmuWiz-first with transparent legacy reuse:

1. Look for the EmuWiz path (`~/.config/emuwiz`, `~/.local/share/emuwiz`)
   first.
2. If absent, detect the legacy ArchiveFS path
   (`~/.config/archivefs`, `~/.local/share/archivefs`) and reuse it.
3. If neither exists (a fresh EmuWiz install), use the EmuWiz path.

Resolution never copies, moves or overwrites anything, so it is idempotent and
cannot destroy conflicting destination data. The database, index, cheat-source
caches, artwork, BSFree catalogue, DAT journals, library-view manifests,
emulator-profile memory and GUI-mode file all follow the effective directory,
so a legacy user's complete data set stays reachable. A future migration pass
may move legacy data into the EmuWiz directories; until then, the legacy
directories remain the active ones for existing users. The `install.sh`
config-directory choice mirrors the same rule.

## Environment variable compatibility

`EMUWIZ_LOG` is the new log-level variable; the legacy `ARCHIVEFS_LOG` is still
honoured, with `EMUWIZ_LOG` winning when both are set. All other
`ARCHIVEFS_*` variables are internal test subprocess protocols and are
unchanged.
