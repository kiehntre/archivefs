# EmuWiz v0.8.0-alpha ("Alpha 2.0") release checklist

Concise checklist for this release only. Do not tag `v0.8.0-alpha` until
every box below is checked against the exact final commit. This does not
replace [`docs/release-checklist.md`](release-checklist.md) or
[`docs/release-checklist-alpha-1.1.md`](release-checklist-alpha-1.1.md),
which record other, already-shipped releases.

## Source and version

- [ ] Workspace version is `0.8.0-alpha` for `archivefs-core`, `archivefs-cli`,
      and `archivefs-gui` (`Cargo.toml` `[workspace.package].version`) - already
      true as of this writing; reconfirm on the exact commit to be tagged.
- [ ] `Cargo.lock` reflects the same version for all three workspace members.
- [ ] CLI and GUI `--version` resolve from Cargo metadata, not a hardcoded
      string.
- [ ] `CHANGELOG.md`'s `## v0.8.0-alpha (unreleased)` heading drops
      `(unreleased)` and gains the tag date only at tag time, not before.
- [ ] `docs/releases/v0.8.0-alpha.md`'s "not yet tagged or published" status
      note is removed/updated only at tag time.
- [ ] `README.md`'s release-status paragraph is updated to point at
      `v0.8.0-alpha` as the current published release only after tagging.
- [ ] No schema/migration change shipped in this release (confirm against
      the actual diff, not assumption - this release's scope is symlink-based
      Library Views, repair-center persistence via existing journal files,
      and read-only DAT/media recognition changes; no new SQLite migration
      or database column is expected).
- [ ] No ROM, disc image, optional BSFree database, secret, or build output
      is tracked or staged.

## Automated gates

Run from a clean clone:

- [ ] `git diff --check`
- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo audit`
- [ ] `scripts/security-scan.sh`
- [ ] `bash tests/test_install.sh`
- [ ] `scripts/build-release.sh` (canonical release artifact build)
- [ ] `scripts/check-version-consistency.sh` against the built binaries,
      artifact, and checksum
- [ ] `scripts/verify-release-artifact.sh` (canonical artifact verifier)
- [ ] Built `emuwiz-cli --version` and `emuwiz --version` both report
      `0.8.0-alpha`.

Record pass/fail for each gate in the release PR before tagging.

## Manual smoke gates

Each journey below must be executed against the exact commit being
released, using disposable fixtures (never irreplaceable ROMs), and
recorded with tester, date, commit SHA, and outcome.

### A. Library View plan/apply/idempotence/rollback

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

- [ ] Import/inspect a real-world Logiqx DAT carrying the standard
      `PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN"` DOCTYPE;
      confirm the diagnostic reads as DTD-recognised (resolved or
      unavailable, per whether a local copy exists), never as a generic
      "inert text" note.
- [ ] Confirm no diagnostic message ever claims DTD schema validation
      occurred.

## Publication gate

- [ ] All automated gates above pass on the exact commit to be released.
- [ ] All five manual smoke journeys (A-E) are executed and signed off
      against that same commit.
- [ ] Explicit authorization received to merge, tag, and publish
      `v0.8.0-alpha`.
- [ ] Annotated tag is exactly `v0.8.0-alpha` and points at the final main
      release commit.
- [ ] Published assets are exactly the verified
      `archivefs-v0.8.0-alpha-x86_64-linux.tar.gz` archive and its checksum.

Do not create the tag until every box above is checked against the exact
final commit.
