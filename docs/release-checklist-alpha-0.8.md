# EmuWiz v0.8.0-alpha ("Alpha 2.0") release checklist

Concise checklist for this release only. Do not tag `v0.8.0-alpha` until
every box below is checked against the exact final commit. This does not
replace [`docs/release-checklist.md`](release-checklist.md) or
[`docs/release-checklist-alpha-1.1.md`](release-checklist-alpha-1.1.md),
which record other, already-shipped releases.

## Source and version

- [x] Workspace version is `0.8.0-alpha` for `archivefs-core`, `archivefs-cli`,
      and `archivefs-gui` (`Cargo.toml` `[workspace.package].version`) -
      confirmed via `scripts/check-version-consistency.sh` at runtime commit
      `703992d9e3ca686eb431741856609784ab6428e6`. Documentation-only commits
      after this one do not touch `Cargo.toml`/`Cargo.lock`, so this remains
      true on the final tag commit without needing to be reconfirmed there.
- [x] `Cargo.lock` reflects the same version for all three workspace members
      - same basis as above.
- [x] CLI and GUI `--version` resolve from Cargo metadata, not a hardcoded
      string - unchanged code-structure fact, unaffected by this release.
- [x] `CHANGELOG.md`'s `## v0.8.0-alpha (unreleased)` heading drops
      `(unreleased)` and gains the tag date - done in this documentation
      commit (`## v0.8.0-alpha (2026-08-18)`).
- [x] `docs/releases/v0.8.0-alpha.md`'s "not yet tagged or published" status
      note is removed/updated - done in this documentation commit.
- [ ] `README.md`'s release-status paragraph is updated to point at
      `v0.8.0-alpha` as the current published release - **intentionally not
      done in this commit.** Per this file's own instruction, this happens
      only after tagging, in a separate follow-up commit.
- [x] No schema/migration change shipped in this release (confirmed against
      the actual diff - this release's scope is symlink-based Library Views,
      repair-center persistence via existing journal files, and read-only
      DAT/media recognition changes; no new SQLite migration or database
      column was introduced).
- [x] No ROM, disc image, optional BSFree database, secret, or build output
      is tracked or staged - consistent with `scripts/security-scan.sh`'s
      result below (489 tracked files, no credential-shaped secrets).

## Automated gates

Run from a clean clone:

Proven against **runtime commit `703992d9e3ca686eb431741856609784ab6428e6`**
(the last commit that touches source/runtime code; no runtime code changes
land after it in this release):

- [x] `git diff --check` - PASS
- [x] `cargo fmt --all --check` - PASS
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
      - PASS
- [x] `cargo test --workspace` - PASS (1438 GUI tests included, 0 failures)
- [x] `cargo audit` - PASS, no vulnerability reported
- [x] `scripts/security-scan.sh` - PASS, 489 tracked files, no
      credential-shaped secrets
- [x] `bash tests/test_install.sh` - PASS, 249 passed, 0 failed, including
      the installer controlling-TTY regression check

**Artifact-dependent gates - PENDING RERUN, not yet valid for tagging.**
The final tag commit will be a documentation-only descendant of
`703992d` (see "Documentation-only final commit" below). `CHANGELOG.md` is
one of the files `scripts/build-release.sh` packages directly into the
release tarball, so this documentation commit changes the artifact's bytes.
The build/verify chain below was already run once, against `703992d`
(SHA256 `7ff5814f1a660be70713946622f10bb25d2160d21bc4c07e217ddc70c0d4aa37`,
byte-for-byte reproducible per `scripts/compare-release-builds.sh`), but
that result belongs to a commit that will not be the tagged commit. It must
be rerun, and a new checksum recorded, against the actual final commit
before tagging:

- [ ] `scripts/build-release.sh` (canonical release artifact build) - rerun
      pending on the final documentation commit.
- [ ] `scripts/check-version-consistency.sh` against the built binaries,
      artifact, and checksum - rerun pending on the final documentation
      commit.
- [ ] `scripts/verify-release-artifact.sh` (canonical artifact verifier) -
      rerun pending on the final documentation commit.
- [ ] `scripts/compare-release-builds.sh` (byte-for-byte reproducibility) -
      rerun pending on the final documentation commit.
- [ ] Built `emuwiz-cli --version` and `emuwiz --version` both report
      `0.8.0-alpha` - reconfirm as part of the rerun above.

Record pass/fail for each gate in the release PR before tagging.

## Manual smoke gates

Each journey below must be executed against the exact commit being
released, using disposable fixtures (never irreplaceable ROMs), and
recorded with tester, date, commit SHA, and outcome.

**Recorded result:**

| Tester  | Date       | Runtime commit                              | A    | B    | C    | D    | E    |
|---------|------------|----------------------------------------------|------|------|------|------|------|
| davedap | 2026-08-18 | `703992d9e3ca686eb431741856609784ab6428e6`   | PASS | PASS | PASS | PASS | PASS |

**Documentation-only final commit - smoke not repeated.** The commit that
will actually be tagged is a documentation-only descendant of
`703992d` (this release-doc finalization commit changes only `CHANGELOG.md`,
`docs/releases/v0.8.0-alpha.md`, and this checklist file - no runtime or
source-code file). The table above is therefore recorded against
`703992d`, not against the literal final tag commit, and that distinction is
intentional, not an oversight: since no runtime code changes between
`703992d` and the final tag commit, re-executing journeys A-E solely to
change which commit SHA is written down would exercise identical runtime
behavior and add no evidence. Do not read the table above as a claim that
A-E were executed against the exact commit ultimately tagged - they were
not, and were not required to be, precisely because that commit is
runtime-identical to `703992d`. If any further runtime-code change lands
after this documentation commit for any reason, this table becomes stale
and A-E must be re-executed against the new runtime commit before tagging.

### A. Library View plan/apply/idempotence/rollback

**Outcome: PASS** - see the recorded result table above.

Exercises `crates/archivefs-core/src/library_views.rs` and the Library
Views GUI page end to end, with a disposable source folder and a disposable
destination (never a real archive under management).

- [ ] Preview a `Generic` and a `Romm` profile view against the same
      disposable source; confirm the planned paths match the expected
      `{platform}/{filename}` / `roms/{slug}/{filename}` shapes.
- [ ] Apply the view; confirm every generated destination object is a
      symlink (never a copy), every symlink target resolves inside the
      real source root, and the source files are byte-for-byte and
      mtime-for-mtime unchanged.
- [ ] Apply the same view a second time with nothing changed; confirm it is
      fully idempotent (0 unnecessary Create/Repair/Remove).
- [ ] Confirm destination containment holds even with a pre-existing
      symlinked ancestor placed at the destination root (this is a
      regression check for the Apply containment hardening in this
      release - see the `library_views` symlink-escape tests for the exact
      shape).
- [ ] Delete the archive a view's symlink points at, then re-apply; confirm
      Apply fails closed (refuses/reports) rather than creating a dangling
      link, and that no unrelated file is touched.
- [ ] Remove the view; confirm only the generated symlinks are removed and
      the original archives remain exactly where they were.

### B. GUI "Scan library for repairs" and skipped-file explanations

**Outcome: PASS** - see the recorded result table above. During this
journey, a real bug was found and fixed: a Sources-page scan's
`ScanPersistSummary` (including skipped-file detail) was discarded instead
of feeding `DatabaseState::Ready.last_scan_summary`, so the Database
Status "Skipped files -> Inspect..." control was unreachable after a
Sources-page scan (only reachable via a separate Database Status ->
"Scan library" run). The fix is already part of runtime commit
`703992d9e3ca686eb431741856609784ab6428e6`, which this journey's recorded
PASS is against.

Exercises the Repair Review page's scan action and the skipped-files
drill-down added this release.

- [ ] From Repair Review, choose a registered DAT source and a library
      folder, and run "Scan library for repairs"; confirm the GUI stays
      responsive while the scan runs and shows Scanning -> Completed status.
- [ ] Confirm the resulting plan loads directly into Repair Review with the
      normal Safe/Needs Review/Blocked categories and counts.
- [ ] Force a scan failure (e.g. a nonexistent DAT path) and confirm no
      stale or half-loaded plan is shown, and any previously loaded plan
      (if one was open) is left untouched.
- [ ] With a source folder containing files that are skipped for both
      reasons (unsupported extension and ambiguous platform - e.g. an
      unrecognised extension plus an uncorroborated `.gen`/`.bin`/`.md`),
      confirm the skipped-files drill-down shows both reasons with correct
      paths, and that the aggregate counts still match the exact total even
      when the detail list would be capped.
- [ ] Confirm nothing in this journey mutates a ROM file - the scan and the
      drill-down are read-only; only an explicit Apply from the resulting
      plan may rename anything.

### C. Rename transaction restart/recovery (reconciliation fix)

**Outcome: PASS** - see the recorded result table above.

Exercises the transaction-level reconciliation fix in
`crates/archivefs-core/src/dat/rename_apply/reconcile.rs`.

- [ ] Apply a small rename batch to completion; confirm the journal is
      recorded `Applied`.
- [ ] Restart the app (a fresh page load, not just re-rendering); confirm
      the transaction is rediscovered showing `Applied` and offers rollback.
- [ ] Roll back the rediscovered transaction; confirm files return to their
      original names/content.
- [ ] If practical, reproduce a transaction journal manually stuck at
      `Applying` with an already-`Applied` entry (see the reconcile.rs unit
      tests for the exact fixture shape) and confirm a restart reconciles
      it to `Applied` rather than leaving it stuck.

### D. Real-world C128 / Neo Geo CD / RomM SMS re-validation

**Outcome: PASS** - see the recorded result table above, including the
stray-`.chd`-outside-a-recognised-folder fail-closed check.

Re-confirms the real-world validation already performed earlier this cycle
against the exact commit being tagged, not just against an earlier commit
in the branch's history.

- [ ] C128: scan and apply a real (or realistic disposable) `.d64`/`.g64`
      collection through a RomM-profile Library View; confirm symlink
      count, zero broken links, and zero source mutation.
- [ ] Neo Geo CD: same, for a `.chd` collection under a `neocdz`-named
      folder; confirm the folder alias resolves the platform and the view
      applies cleanly.
- [ ] RomM SMS: confirm a RomM-backed identity/Library View flow still
      resolves correctly end to end for a Master System catalogue.
- [ ] Confirm a `.chd` file placed outside any recognised folder still
      resolves no platform (fail-closed check, not a regression from the
      above).

### E. Trusted DTD diagnostics sanity

**Outcome: PASS** - see the recorded result table above.

- [ ] Import/inspect a real-world Logiqx DAT carrying the standard
      `PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN"` DOCTYPE;
      confirm the diagnostic reads as DTD-recognised (resolved or
      unavailable, per whether a local copy exists), never as a generic
      "inert text" note.
- [ ] Confirm no diagnostic message ever claims DTD schema validation
      occurred.

## Publication gate

- [ ] All automated gates above pass on the exact commit to be released -
      **not yet true.** The artifact-dependent gates in "Automated gates"
      are pending rerun against the final documentation commit (see that
      section); this box cannot be checked until that rerun passes.
- [ ] All five manual smoke journeys (A-E) are executed and signed off
      against that same commit - **not literally true, by design.** A-E
      were executed against runtime commit `703992d`, the final tag
      commit's runtime-identical parent; see the "Documentation-only final
      commit" note under "Manual smoke gates" for why this is not being
      treated as a gap requiring re-execution. This box is intentionally
      left unchecked rather than marking a claim about "that same commit"
      true when it is not literally the case.
- [ ] Explicit authorization received to merge, tag, and publish
      `v0.8.0-alpha`.
- [ ] Annotated tag is exactly `v0.8.0-alpha` and points at the final main
      release commit.
- [ ] Published assets are exactly the verified
      `archivefs-v0.8.0-alpha-x86_64-linux.tar.gz` archive and its checksum
      - must be the checksum from the artifact rebuilt against the final
      documentation commit, not the `7ff5814f...` checksum recorded against
      `703992d`.

Do not create the tag until every box above is checked against the exact
final commit.
