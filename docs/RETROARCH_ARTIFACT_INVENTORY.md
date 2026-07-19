# RetroArch Cheat and Patch Artifact Inventory

`archivefs retroarch-patch-preview` includes a bounded, read-only inventory
of existing RetroArch cheat and soft-patch artifacts. It answers whether a
previewed destination is empty or occupied and reports other supported files
found in the same configured locations without installing, enabling, applying,
renaming, copying, deleting, or creating anything.

The inventory is part of the existing command rather than a second discovery
pass or a write-capable patch-manager phase. Human output follows the preview;
JSON is available at `artifact_inventory`:

```console
archivefs retroarch-patch-preview
archivefs retroarch-patch-preview --json
```

## Scope

The only recognized artifact extensions are:

- RetroArch cheat files: `.cht`
- RetroArch soft patches already used by the destination preview: `.ips`,
  `.bps`, `.ups`, and `.xdelta`

Extension matching is ASCII case-insensitive for discovery. Real path bytes,
not a case-folded or lossy rendering, remain the identity. Patch payloads are
never parsed or executed. The inventory reports their path, filename, size,
file type, symlink state, and association evidence only.

For `.cht`, ArchiveFS performs a bounded text read and reports only:

- the first non-empty `cheatN_desc` value;
- the declared `cheats` count, when valid;
- the number of distinct bounded `cheatN_*` entries observed;
- how many entries contain `cheatN_enable = true` and whether any are enabled;
- malformed line numbers; and
- whether the bounded metadata view appears complete.

Cheat codes and payload values are not interpreted or executed. ArchiveFS does
not change any enable value.

## Filesystem boundaries

Inventory reuses `ReadOnlyHostFilesystem`, which has no create, write, rename,
delete, execute, or process-control method. Its probes, bounded reads, and
bounded directory listings use final-component no-follow metadata. A symlink is
reported as a symlink and its target is not opened. Ancestor components retain
the documented POSIX race limitation described in
[`RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md); no claim of a fully
race-free hostile-filesystem snapshot is made.

Limits are fixed in code:

- cheat file read: 2 MiB;
- patch metadata inspection threshold: 1 MiB (bodies are not read at any size);
- entries in one directory listing: 8,192;
- total artifact findings: 8,192;
- directories visited per profile: 8,192; and
- cheat entry indices per file: 16,384.

Configured cheat roots are traversed recursively within those bounds. Patch
directories are limited to the unique parent directories of destinations
already derived for present catalogue games; ArchiveFS does not crawl arbitrary
content roots or the whole home directory. A limit or inaccessible directory
sets `complete: false` and/or emits a structured diagnostic. Partial results are
never presented as a complete inventory.

No external command, RetroArch process, core, archive extractor, or network
client is invoked.

## Associations

Associations are evidence, not ownership claims. The tiers are:

1. `exact`: the artifact's byte path is an exact expected destination.
2. `strong`: a cheat filename and its core-directory name both match one
   expected game/core destination.
3. `weak`: filename-only or normalized-filename-only evidence identifies one
   game.
4. `ambiguous`: multiple catalogue games tie at the best available tier.
5. `unsupported`: no supported association can be made.

The expected destinations already incorporate the existing playlist and core
selection rules. A finding therefore carries the relevant catalogue game,
compact playlist evidence, installed-core stem evidence, and expected paths.
ArchiveFS never silently chooses one game from an ambiguous set. A supported
file that cannot be associated remains visible; it is not labelled malformed or
corrupt merely because ArchiveFS lacks identity evidence.

## States

`ArtifactConflictState` uses stable lower-snake-case values:

- `empty`: an expected path does not exist;
- `occupied`: a regular file occupies an expected destination;
- `matched`: an existing finding associates with one game;
- `duplicate`: more than one artifact associates with the same profile, kind,
  and catalogue game;
- `conflicting`: an expected or discovered artifact path is a symlink,
  directory, or other wrong type;
- `orphaned`: a regular supported artifact has no catalogue association;
- `ambiguous`: more than one game ties;
- `unsupported`: metadata access failed and ArchiveFS cannot classify further.

Side-by-side destination and finding arrays are intentional: `destinations`
answers whether every proposal is empty/occupied/conflicting, while `findings`
retains every existing supported artifact, including orphans.

## JSON contract

`artifact_inventory.format_version` is `1`, independent of the enclosing
preview's format version. Its exact top-level keys are:

```text
format_version
read_only
complete
findings
destinations
diagnostics
summary
```

Enums use explicit lower-snake-case Serde names. Filesystem paths use the
existing `EncodedPath { display, lossy }` representation; a lossy display is
never reconstructed and used as path identity. Diagnostics use stable `code`
and `severity` fields instead of making prose the only machine-readable result.

Adding `artifact_inventory` is additive to `RetroArchAdvisoryPlan` format 1.
The embedded RetroArch environment retains its own format version. The preview
plan ID continues to identify destination derivation and matching; local
inventory churn does not turn it into an installation or ownership token.

## Non-goals

This inventory does not:

- install, download, enable, apply, validate, hash, or repair an artifact;
- parse cheat codes or patch payloads;
- infer that a patch is safe, correct, legal, or compatible;
- mutate RetroArch configuration, playlists, cores, game content, or the
  ArchiveFS catalogue;
- follow artifact symlinks;
- extract or inspect archive members;
- launch RetroArch or execute external commands;
- create manifests, backups, caches, locks, or audit records; or
- add an install, fix, remove, or repair flag.

Any future mutation workflow remains separately gated by
[`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md).

## Related: external cheat catalogue matching

`archivefs retroarch-cheat-catalogue` is a separate, independent read-only
command that matches an *external* local cheat catalogue source (not this
inventory's own already-installed artifacts) against your catalogued games,
reusing this inventory's destinations to answer whether a matched catalogue
cheat is already installed. It adds its own additional bounded read (a
SHA-256 hash comparison) that this inventory deliberately does not perform -
see [`RETROARCH_CHEAT_CATALOGUE.md`](RETROARCH_CHEAT_CATALOGUE.md).
