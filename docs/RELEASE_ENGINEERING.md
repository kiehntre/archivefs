# ArchiveFS release engineering

This document is the maintained release procedure for ArchiveFS. The scripts
described here build and inspect artifacts; they do not create tags, push
branches, or publish releases.

## Release contract

The canonical Linux bundle is:

```text
archivefs-v<VERSION>-<ARCH>-linux.tar.gz
archivefs-v<VERSION>-<ARCH>-linux.tar.gz.sha256
```

`VERSION` comes from Cargo workspace metadata. The supported architecture names
are currently `x86_64-linux` and `aarch64-linux`. The archive contains exactly
one same-named root directory with:

```text
archivefs-cli          0755
archivefs-gui          0755
install.sh             0755
README.md              0644
CHANGELOG.md           0644
LICENSE                0644
config.toml.example    0644
```

Every tar member has numeric UID/GID `0:0`. Entries are name-sorted, timestamps
come from `SOURCE_DATE_EPOCH` (the release commit timestamp by default), gzip
stores no original filename or timestamp, and compiler paths are remapped to
neutral `/build/...` prefixes.

## Required local tools

- Git
- Bash
- the Rust toolchain pinned by `rust-toolchain.toml`, including Cargo
- GNU tar and gzip
- Python 3 with the standard `tarfile` data-filter API
- `install`, `sha256sum`, and `strings` (GNU/binutils implementations on Linux)
- optional: `cargo-audit` for the dependency-security gate

Run release tooling from a Linux checkout. The canonical scripts reject a dirty
repository. Their default outputs live below ignored `target/`; an explicit
output directory may be anywhere, but should be empty.

## One-command release build

From a clean checkout of the intended release commit:

```sh
scripts/build-release.sh --output-dir "$PWD/target/release-artifacts"
```

The builder:

1. verifies the worktree is clean;
2. reads one consistent ArchiveFS version from `cargo metadata`;
3. runs `cargo build --workspace --release --locked` with neutral compiler-path
   remapping;
4. stages only the required files with fixed permissions;
5. writes a deterministic, sorted, numeric-owner archive and a one-record
   SHA-256 file;
6. invokes the independent verifier;
7. safely extracts into a temporary directory; and
8. runs both extracted binaries with `--version`. The GUI takes its version-only
   exit path, so no window or display connection is opened.

Use `--target-dir DIR` when the Cargo build itself must be isolated, such as a
reproducibility run. Existing artifacts are not overwritten; use an empty
output directory for each run.

## Verify an existing artifact

Keep the archive and its `.sha256` file together, then run:

```sh
scripts/verify-release-artifact.sh \
  target/release-artifacts/archivefs-v0.7.0-x86_64-linux.tar.gz
```

The verifier checks the checksum filename and record, exact root layout,
required and unexpected files, file types, modes, numeric ownership, path
traversal, extracted path containment, credential-shaped strings, maintainer
paths, common build paths, and both binary versions. It returns non-zero on any
failure.

Its negative regression suite uses generated fixtures and must also pass:

```sh
scripts/test-release-artifact-verifier.sh \
  target/release-artifacts/archivefs-v0.7.0-x86_64-linux.tar.gz
```

This proves rejection of a bad checksum, unexpected member, traversal path,
unsafe executable mode, and embedded maintainer path. It never extracts an
unvalidated member.

## Reproducibility

Run two independent release builds with separate Cargo target directories:

```sh
scripts/compare-release-builds.sh \
  --output-dir "$PWD/target/reproducibility"
```

The output directory must be empty. The comparison fails unless:

- each archive independently passes the verifier;
- archive bytes are identical;
- checksum files are identical; and
- member order, payload SHA-256, modes, numeric ownership, timestamps, types,
  and sizes are identical.

A green run is evidence only for the tested commit, toolchain, target, and host
class. It is not a universal cross-platform reproducibility claim. If a future
toolchain prevents byte-identical archives, do not weaken the check silently:
record the exact differing metadata/compiler input, retain the payload-manifest
comparison, and document whether only extracted payload reproducibility was
demonstrated.

## Version consistency

The source-only check is:

```sh
scripts/check-version-consistency.sh
```

For a built artifact, also check both binaries and filenames:

```sh
scripts/check-version-consistency.sh \
  --binary-dir target/release \
  --artifact target/release-artifacts/archivefs-v0.7.0-x86_64-linux.tar.gz \
  --checksum target/release-artifacts/archivefs-v0.7.0-x86_64-linux.tar.gz.sha256
```

The workspace packages, CLI output, GUI output, archive, checksum, README
release reference, and current changelog heading must agree. Do not hard-code a
different version in release scripts and do not bump the project version as a
side effect of building.

## Security checks

Run the lightweight tracked-file scan and RustSec audit:

```sh
scripts/security-scan.sh
cargo audit
```

If `cargo-audit` is not installed locally, install it intentionally with
`cargo install cargo-audit --locked`, or rely on the required CI security job.
The scanner targets credential shapes (private keys, GitHub/AWS tokens,
credential assignments, and credential-bearing URLs) rather than flagging
ordinary security vocabulary.

CI installs the current locked `cargo-audit` release and fails on vulnerable
resolved dependencies. There are no ignored advisories at the time this
procedure was written. A future exception must identify the advisory, affected
dependency/path, exposure analysis, expiry/review condition, and tracking issue
in this document and beside the CI command. Never add a blanket ignore or
upgrade dependencies solely to make the report disappear without validating
the repair.

The artifact verifier repeats a focused privacy/credential scan over the actual
payload, including printable strings embedded in both binaries. This is
separate from the tracked-source scan because compiler output can introduce
paths that do not appear in source.

## Continuous integration

`.github/workflows/ci.yml` requires no repository secrets and has distinct jobs:

- **Formatting** — `cargo fmt --all --check`
- **Clippy** — `cargo clippy --workspace --all-targets -- -D warnings`
- **Workspace tests** — `cargo test --workspace`
- **Locked release build** — `cargo build --workspace --release --locked` plus
  binary version consistency
- **Dependency and secret audit** — tracked-file scan plus `cargo audit`
- **Create canonical artifact** — canonical builder, verifier negative suite,
  and version check
- **Verify downloaded artifact** — verifies the uploaded/downloaded CI payload,
  not just the files left in the producing job
- **Reproducibility** — two isolated builds and byte/metadata comparison

Rust caches are scoped through `Swatinem/rust-cache`; failed builds are not
saved. Reproducibility deliberately uses independent target directories rather
than a shared target cache. The candidate archive and checksum are uploaded as
a CI artifact for 14 days. Pull-request CI never creates a tag or GitHub
Release and needs only read access to repository contents.

The tag-only `.github/workflows/release.yml` also calls the canonical builder.
It validates that the tag is exactly `v` plus the Cargo workspace version
before publication, preventing a tag/artifact version mismatch.

## Manual validation remains required

Automation does not replace desktop release QA. Before publishing:

1. download the CI artifact into a clean directory and run the independent
   verifier;
2. launch the extracted GUI on a supported desktop and complete the approved
   manual GUI checklist at normal desktop size and 1024×600;
3. run `archivefs-cli doctor` using disposable configuration;
4. exercise installation and rollback only with disposable emulator profiles;
5. confirm no live ROM, production emulator profile, catalogue cache, database,
   or configuration is included or modified; and
6. record the exact commit, artifact SHA-256, host architecture, and QA result.

## Prerelease and stable procedure

Prerelease tags include a SemVer suffix, for example `v0.7.0-alpha` or
`v1.0.0-rc.1`. The release workflow marks these as GitHub prereleases. A stable
release uses `vMAJOR.MINOR.PATCH` with no suffix. In both cases:

1. update the workspace version and release documentation in a dedicated,
   reviewed version-bump change (not in the artifact build);
2. complete all automated and manual gates on the exact commit;
3. confirm the worktree is clean and the branch is pushed;
4. create one annotated tag on that exact commit; and
5. push only that tag, which starts the tag-only release workflow.

For v0.7.0, after every gate and explicit release authorization, the
commands would be:

```sh
git fetch origin
git status --short
git rev-parse HEAD
git tag -a v0.7.0 <VERIFIED_COMMIT_SHA> -m "ArchiveFS v0.7.0"
git push origin v0.7.0
```

These commands are documentation, not authorization to run them. Never move or
overwrite a published tag. Confirm the GitHub Release contains only the
canonical archive and matching `.sha256` file, then download and verify those
published bytes again.

## Rollback and database downgrade warning

Before upgrading an alpha installation, stop ArchiveFS and copy the database
and managed-state directory. Keep the previous verified application artifact
and checksum.

Application rollback means stopping ArchiveFS and restoring the previous
binary bundle. **In-place database downgrade is not supported.** ArchiveFS
v0.7 uses schema 6; older binaries reject a schema-6 database. To run an older
binary, restore the pre-upgrade database copy to a separate compatible path.
Keep the v0.7 database and managed state untouched for later recovery. Never
edit SQLite schema/version fields manually, and do not assume an older binary
can interpret new journals, profile records, or catalogue state.

If a published artifact itself must be withdrawn, preserve the tag, checksums,
and incident evidence; mark the release clearly rather than replacing bytes
under the same filename. Fix forward with a new version and tag.

## Troubleshooting

- **“repository must be clean”** — inspect `git status --short`; release only
  committed source. Outputs below `target/` are ignored.
- **output already exists** — choose a new empty output directory. The builder
  does not overwrite evidence from an earlier run.
- **checksum mismatch** — discard the archive and reacquire/rebuild it; never
  regenerate a checksum for bytes whose origin is uncertain.
- **unexpected member, unsafe path, mode, or owner** — treat this as a packaging
  failure. Do not extract manually to bypass the verifier.
- **version mismatch** — align Cargo workspace version, current changelog,
  documentation, tag, and artifact by reviewing the intended release commit.
- **local path found in a binary** — ensure the canonical builder supplied all
  remap flags and that no wrapper replaced `RUSTFLAGS`; rebuild from a clean
  target directory.
- **reproducibility mismatch** — retain both output directories and compare the
  emitted manifests. Identify compiler bytes versus tar metadata before making
  any claim or changing normalization.
- **cargo-audit finding** — inspect the advisory and dependency path with
  `cargo tree`; repair only with a reviewed compatible dependency change or a
  narrowly documented, time-bounded exception.
