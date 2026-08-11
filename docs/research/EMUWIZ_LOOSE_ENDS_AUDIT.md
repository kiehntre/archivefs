# EmuWiz loose-ends / project-health audit

Audit date: 2026-08-11. Original code baseline: `origin/main` at
`f7c450cad3b89207251dd3b2b4747af1f1e01d42` (merge of PR #29). The report was
refreshed on 2026-08-11 against current `origin/main` at
`7c8d6ea1891d4bd32bcdb0716ff7d998ec08ed83` (merge of PR #33); PR #33 is treated
as landed, and the refreshed active plan is the final major section of this
report. This was a read-only audit; the only workspace change is this report.

## Executive summary

EmuWiz is healthier than its roadmap and current release prose suggest. The
core safety architecture is substantial, tests are numerous, and the recent DAT,
Cheats & Mods, branding, and beginner-workflow work is present on `main`.
The main risk before another large feature cycle is not an unfinished feature;
it is loss of a trustworthy description of what is already shipped.

The highest-value close-out is: correct the security/product docs, add the one
missing Games-only stale-classifier gate, make install/uninstall ownership-safe,
and close or preserve the four open PRs deliberately. The next technical feature
should be a small archive-aware DAT verification slice, not another broad
adapter.

## Open and recent PR state

| PR | State | Audit disposition |
|---|---|---|
| #33 `desktop: add EmuWiz Linux application icon` | Open draft; active review | Keep active and review normally. It contains the app icon, Linux application ID, desktop launcher, hicolor install, and release-verifier work. None is on `main` yet. Do not duplicate it. |
| #32 `[ImgBot] Optimize images` | Open, non-draft | Close unless a maintainer explicitly wants a separately reviewed asset-reencoding change. It rewrites approved branding/platform assets, overlaps #31/#33, and undermines the byte-identity provenance asserted by #31/#33 for negligible savings. |
| #30 `docs: research encrypted Action Replay licensing` | Open draft; research only | Preserve, but do not implement from it. The YELLOW result is still current: scheme-specific constant provenance/permission is unresolved and GPL code must not be copied. After source/legal review, either merge a clearly non-legal-advice research record or close while preserving the commit/tag elsewhere. Do not leave the only durable copy in a temporary worktree. |
| #27 `docs: add EmuWiz compatibility rename research` | Open draft; research only | Superseded in part by merged #26 and `docs/reviews/EMUWIZ_RENAME_AUDIT.md`; it also predates the actual repository rename to `kiehntre/emuwiz`. Rebase and reduce it to genuinely durable migration findings, or close it as superseded. Do not merge it unchanged. |

Recent merged follow-ups still open:

- #31 explicitly deferred app/desktop icon integration; #33 is the correct
  follow-up and remains unlanded.
- #29 delivered the safe Games-only P0, but `classifier_version` is recorded and
  never enforced, and multidisc entries are labelled rather than grouped.
- #28 delivered shared Dolphin dedup and a fixture-proven BSFree Wii path, but
  the shipped BSFree snapshot has no Wii rows; encrypted AR remains blocked.
- #26 delivered the user-facing rename, but current docs still contain old repo
  URLs and stale product claims. Its rename audit also says the GitHub repository
  is intentionally unrenamed, which is now false.
- #24/#25 research was useful and safely merged. Their roadmaps now need status
  annotations so completed P0 work is not mistaken for future work.

## Roadmap reality check

| Area | Reality on current `main` | Classification |
|---|---|---|
| DAT parse/index/audit, source GUI, diagnostics/progress | Implemented and tested | **Done** |
| DAT preferences, rename plan/apply/rollback, canonical organisation | Implemented with generation, identity, no-clobber, journal and rollback gates | **Done** |
| Games-only P0 | Conservative No-Intro structured fields + TOSEC categories/tokens, persisted All/Games policy, downstream gating and GUI counts are present | **Done (P0)** |
| Games-only classifier lifecycle | Plans record `dat-content-p0-v1`, but transaction construction/apply compares only generation, not classifier version | **Partially done** |
| Multidisc behavior | TOSEC token detection retains every part, but there is no set identity, completeness/dependency model, or atomic group selection/apply | **Partially done / B2 deferred** |
| Redump/OMNI_DAT/MAME/Libretro enrichment, overrides/review/export | Research/design only; generic and Redump entries currently remain Unknown absent trusted supported metadata | **Researched only / deferred** |
| PCSX2 safe PNACH provider path | Core and GUI staging/preview/apply/rollback exist (`start_pcsx2_install_preview`, `stage_pcsx2_pnach`); no bundled independently reviewed ordinary-cheat catalogue exists | **Implementation done; provider content blocked** |
| BSFree GameCube supported subset | GUI + CLI preview/apply/rollback implemented | **Done** |
| BSFree Wii supported subset/shared dedup | Implemented and fixture tested, but current BSFree data has no Wii rows | **Done technically; dormant in real data** |
| Encrypted GC/Wii Action Replay | Licensing/provenance research is YELLOW in unmerged #30; no decryptor exists | **Blocked** |
| Mods and new adapters (PPSSPP, DuckStation, RPCS3, etc.) | No general mod pipeline; adapter ideas remain research candidates | **Deferred** |
| Branding rename and approved logo | Product strings and logo are on `main`; compatibility identifiers intentionally remain | **Done** |
| Desktop/app icon integration | Only in active-review #33 | **Still genuinely next, pending review** |
| Archive-aware DAT verification, NES header normalization, ZIP-member verification | No preserved research artifact found on `main`, open PRs, branches, or registered worktrees; current audit hashes each outer file as-is | **Not implemented; research not preserved** |
| Wrong-platform-folder detection | Some signature/folder conflicts are visible and authoritative DAT/RomM conflicts can block organisation; there is no general DAT finding that says an exact match is stored under the wrong platform folder | **Partially done** |
| Performance beyond smoke scale and GUI information architecture | Explicitly deferred pending measurements/usage | **Deferred** |

Obsolete roadmap text:

- `ROADMAP.md:197` says the current workspace is the v0.7 release branch; it is
  now post-v0.7 `main` with PRs #24–#31 merged.
- `ROADMAP.md:224-225` lists PCSX2 safe PNACH merge as next and read-only. The
  mutation pipeline and GUI entry point exist; the honest remaining blocker is
  a licensed, reviewed provider.
- `ROADMAP.md:268-274` describes DAT/community sources and patch/artwork sources
  as unimplemented research despite current DAT sources, RetroArch/Dolphin/Xenia
  retrieval, RomM, and artwork support.

## Ranked cleanup board

### P0 — before the next major feature

| File / area | Why it matters | Current status | Recommended next action | Work type |
|---|---|---|---|---|
| `SECURITY.md:30-55`, `docs/security.md` | The public security policy says the only network access is PCSX2 metadata and that patch handling does not write files. Current EmuWiz has multiple explicit network clients and journaled emulator-file mutation. A stale security boundary is worse than an incomplete feature list. | Claims are demonstrably false; no new vulnerability inferred. | Rewrite around actual opt-in network surfaces, token handling, cache/extraction boundaries, and current mutation engines. Keep guarantees narrow and test-backed. | Review + documentation |
| `install.sh:109-119,170-179` | Install uses `cp -f`/`ln -sf`; uninstall removes five fixed names in any supplied prefix without proving EmuWiz created them. A user-owned same-name binary can be overwritten or deleted. | Evidence-backed ownership assumption; temp-prefix tests do not cover pre-existing same-name foreign files. | Add an install manifest or content/symlink ownership checks; refuse/backup foreign targets and uninstall only owned entries. Add adversarial shell tests. Coordinate with #33's desktop files. | Implementation + review |
| `crates/archivefs-core/src/dat/classification.rs:12-17`; `dat/rename_plan/model.rs`; `dat/rename_apply/executor.rs`; `dat/rom_organisation/transaction.rs` | The comment promises recorded classifier versions prevent a later classifier silently reinterpreting reviewed actions, but apply gates only on plan generation. | `classifier_version` is stored in plans, not transactions and not compared. | Put the classifier version into the reviewed transaction/apply context, reject a mismatch against `CLASSIFIER_VERSION`, and test rename plus organisation paths. Document the lifecycle. | Implementation |
| `README.md:162-188`, `CHANGELOG.md:98`, `docs/ADAPTER_SUPPORT_MATRIX.md:31`, `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md:170-181`, `ROADMAP.md:195-230` | Current user/contributor docs call BSFree browse-only, PCSX2 preview-only/unwired, and old roadmap items next. This will cause duplicate work and incorrect release decisions. | Already solved in code; docs stale. | One evidence-based current-doc reconciliation PR. Mark snapshot/review docs historical rather than rewriting their historical findings. | Documentation + review |
| Open PRs #27/#30/#32/#33 | Four open PRs have four different purposes, but only #33 is active implementation review. Unresolved research/assets will keep contaminating branch and release decisions. | #27/#30 drafts, #32 bot PR, #33 active draft. | Close #32; decide preserve-vs-close for #27/#30; finish review of #33 without treating it as landed. | Review + cleanup |

### P1 — soon

| File / area | Why it matters | Current status | Recommended next action | Work type |
|---|---|---|---|---|
| `crates/archivefs-core/src/dat/sources/audit_run.rs`, `dat/audit.rs`, `identity_source/hashing.rs` | DAT audit hashes the outer file. A ZIP is compared as a ZIP, and a headered NES dump is compared byte-for-byte; this produces avoidable false negatives against member/header-normalized DAT evidence. | No archive-aware evidence layer; no research document found. | First preserve a bounded design: ZIP single/multi-member identity, member paths/types/limits, NES iNES/headerless normalization rules, provenance and verdict wording. Then implement the smallest read-only slice (NES-in-ZIP is a good candidate). | Research, then implementation |
| Games-only B2 in `dat/classification.rs` and downstream plan models | Labelling `(Disc N of M)` as `RequiredMultidiscPart` keeps parts visible but does not prove a complete set or make selection/rename/organisation atomic. | Partially done. | Add explicit group identity only from trusted source relations or strict proven tokens, completeness diagnostics, and group-level eligibility/action tests. Do not infer groups from loose words like “Disc”. | Research + implementation |
| DAT platform diagnostics (`DatAuditRequest.platform`, platform identity resolution, organisation plan) | Exact DAT evidence can identify a different platform than the containing folder, but there is no general “wrong platform folder” audit result. | Conflict machinery exists in adjacent subsystems; user-facing DAT finding absent. | Add a read-only diagnostic that keeps audit truth separate from folder placement and never auto-moves. Feed it into organisation review only after explicit resolution. | Implementation |
| `README.md:203-245`, `docs/RELEASE_ENGINEERING.md`, scripts | Release links still use `kiehntre/archivefs`; release bundles/scripts still use `archivefs-v*`; current repo is `kiehntre/emuwiz`. Historical artifact names and binary aliases are valid compatibility surfaces, but current URLs and future branding need an explicit decision. | Mixed current/legacy naming. | Update current repo URLs immediately. Decide once whether future archive filenames remain compatibility names or become `emuwiz-v*`; if renamed, accept/verify the old form rather than breaking old artifacts. | Review + documentation/implementation |
| `README.md`, `CHANGELOG.md`, `docs/BSFREE_*`, `docs/ADAPTER_SUPPORT_MATRIX.md` release truth | The “v0.7” section now mixes a frozen release with post-release main behavior. | Drifted release/current-main distinction. | Separate “latest release” from “current main/unreleased”; do not retroactively rewrite historical release notes. | Documentation |
| Filesystem mutation and live QA | Rename/organisation and cheat transactions have deep tempdir tests, but coverage is mostly simulated filesystems. | Strong unit/e2e fixtures; weak real filesystem/emulator integration. | Add a small Linux integration matrix for renameat2/no-clobber, symlink swaps, permissions, cross-device refusal, interrupted journals, and real emulator config samples. | QA implementation |
| Archive/platform/DAT fixture quality | Archive and platform tests are extensive but synthetic; 7z/RAR mounting, real DAT variants, header normalization and wrong-folder scenarios have weak live corpus coverage. | Unit-heavy. | Add redistributable golden fixtures and optional live tests with exact provenance; keep CI bounded. | QA + research |
| GUI beginner workflows | Headless render/state tests are extensive; X11 was manually exercised, but Wayland and end-to-end mouse/keyboard first-run/install/undo flows remain thin. | Partial manual QA. | Run a release-binary checklist on X11 and Wayland, narrow and record beginner journeys, cancellation, and rollback. | Manual QA + test automation |
| Research preservation | #30 is remote/committed but unmerged; #27 is remote/committed but stale; no archive-aware verification research was found. | Mixed. | Preserve reviewed research under `docs/research/` with base commit, sources, confidence and explicit non-implementation status. Never leave the only copy under `/tmp`. | Review + cleanup |

### P2 — later

| File / area | Why it matters | Current status | Recommended next action | Work type |
|---|---|---|---|---|
| `docs/research/CHEATS_MODS_EXPANSION_RESEARCH.md` P1/P2/P3 | PPSSPP, DuckStation, RPCS3 and device decryptors expand coverage but add new identity/format/safety surfaces. | Research candidates only. | Revisit only after existing pipelines, docs and QA are coherent. Keep browse-only formats browse-only. | Research |
| Encrypted AR implementation | Coverage gain is attractive, but YELLOW licensing/provenance is a hard prerequisite. | Blocked. | Obtain authoritative permission/provenance for scheme-specific constants and independent known-answer vectors; then require a fresh-room implementation review. | Legal/provenance research, then implementation |
| `docs/POST_V0.7_GUI_FOLLOWUPS.md`, “More settings coming later” in GUI | Navigation consolidation and more settings are product-shape decisions, not correctness defects. | Intentionally deferred. | Use observed user data before redesign. | Research/design |
| Deprecated egui compatibility APIs (`docs/DEPENDENCY_SECURITY.md`) | Maintenance debt, not a current advisory. | Intentional allowance. | Migrate in a dedicated visual-regression PR. | Implementation |
| Larger-catalogue performance | Current limits are explicit; no evidenced bottleneck was found in this audit. | Deferred pending measurements. | Profile real workloads before optimizing. | Research |

### DONE / close-out only

- The case-sensitive `TODO`/`FIXME`/`HACK` search found no live code debt
  markers. The only “hack” hit is descriptive “widescreen-hack” prose.
- GUI “follow-up” comments for Select all visible, text-field context menus,
  clipboard failure handling and Unmount selected confirmation annotate work
  that is already implemented. They are stale/historical comments, not open
  defects; optionally rewrite them as provenance-neutral rationale.
- “Later” in installer prompts, rollback guidance, retry copy and cache docs is
  ordinary user wording, not a deferred engineering item.
- `ARCHIVEFS_*`, package/crate names, config/data fallbacks, persisted fields,
  provider IDs, executable aliases, managed block/section markers and historical
  docs are intentional compatibility/history and must not be mass-renamed.
- PR #31's approved logo is landed. App/desktop use belongs only to #33 until
  that PR is reviewed and merged.

## Branding / desktop / release audit

- Repository: GitHub is now `kiehntre/emuwiz`; `README.md:205,210-211` still
  points at `kiehntre/archivefs`. Fix current URLs, while retaining historical
  artifact filenames where necessary.
- README/logo: EmuWiz title and approved local logo are correct on `main`.
- App icon/desktop: absent on `main`; present only in active draft #33. Do not
  report it as shipped.
- Installer: EmuWiz primary binaries plus legacy aliases are correct in intent;
  ownership-safe overwrite/uninstall is the outstanding safety issue.
- Release naming: binaries are EmuWiz-first, bundles remain `archivefs-v*`.
  This is no longer merely an internal crate name and needs an explicit future
  compatibility decision.
- Examples/templates: current user-facing command examples are mixed between
  `emuwiz-cli` and legacy `archivefs-cli`; update living docs, leave historical
  docs and explicit compatibility examples alone.
- Issue templates: no material stale ArchiveFS branding found.

No genuinely stale user-facing `ArchiveFS` product label was found outside
historical/compatibility/ownership-marker contexts. The stale branding is in
URLs, release artifact naming, and documents that describe the old repository
decision—not in live GUI/CLI labels.

## Worktree / build hygiene

Repository metadata reports 26 worktree registrations:

- Active: `/home/davedap/archivefs` (PR #33 branch) and
  `/tmp/opencode/emuwiz-ar-licensing` (PR #30 research).
- Obviously obsolete/superseded: the merged feature/review worktrees under
  `/home/davedap/archivefs-*`, plus `/home/davedap/emuwiz-bsfree-wii` and
  `/home/davedap/emuwiz-rename`. They are clean, but many track branches already
  merged into `main` and are tens of commits behind.
- Prunable registrations: the missing PR13 scratch worktree under
  `/tmp/claude-1000/.../archivefs_pr13/worktree` and missing
  `/tmp/opencode/archivefs-live-ux`.
- Build duplication: `/home/davedap/archivefs/target` is about 80 GiB and
  `/home/davedap/emuwiz-bsfree-wii/target` about 28 GiB. The latter belongs to a
  merged/superseded worktree and is the clearest likely-safe cleanup candidate
  after confirming no process uses it.
- Branches: 49 remote branches are exact ancestors of `origin/main` and are
  straightforward deletion candidates after normal maintainer confirmation.
  A further 21 are not ancestors; many are squash-merged/superseded feature
  branches, but must be compared before deletion. `imgbot`, #27, #30 and #33
  are live PR heads and must be handled through their PR decisions first.

No cleanup was performed.

## Test / QA gap priorities

1. Real filesystem mutation: cross-device mounts, permission failures, symlink
   swaps, power/interruption journal recovery, and no-clobber behavior outside a
   tempdir model.
2. Archive handling: real ZIP/7z/RAR edge corpus, archive-member identity, bomb
   limits, duplicate/case-folded member names and unsupported special entries.
3. Install/uninstall: foreign same-name targets, symlinked prefixes, partial
   install failure, upgrade/downgrade, and (after #33) desktop/icon ownership.
4. Compatibility migration: both EmuWiz and legacy directories with partial or
   conflicting state, old journals/backups, binary aliases and downgrade.
5. Platform detection: signature-versus-folder disagreement across more than
   Atari ST/Mega Drive, with an explicit wrong-folder user finding.
6. DAT normalization: real publisher fixtures, headered/headerless NES,
   ZIP-member matching, multidisc completeness, and classifier-version changes.
7. GUI beginner flows: real release binary, first run through source scan/DAT
   verify/install/undo on X11 and Wayland, including cancellation and errors.

## Documentation drift: exact claims

| File | Stale claim | Current evidence |
|---|---|---|
| `SECURITY.md:32-35` | Only PCSX2 metadata fetch uses the network. | RetroArch/Dolphin/Xenia/GameHacking/BSFree retrieval and RomM clients exist. |
| `SECURITY.md:52-55` | Patch preview does not write files/configuration. | Shared transactions and PCSX2/Dolphin/Xenia/RetroArch/BSFree install paths write only after preview/confirmation. |
| `README.md:128,165,185-188` | PCSX2 remains preview-only; BSFree has no Install action/is browse-only. | GUI PCSX2 install preview/apply entry point and BSFree GameCube/Wii apply paths exist. |
| `CHANGELOG.md:98-99` | Current v0.7 section says BSFree neither converts nor installs. | Post-v0.7 main merged #21/#28. The real error is mixing release and current-main scope. |
| `docs/ADAPTER_SUPPORT_MATRIX.md:31,42-44` | PCSX2 active GUI not wired. | `main.rs:10973-11093,14506` wires preview and apply flow. |
| `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md:179-181` | GUI apply control absent. | Same document's earlier section and current GUI code say it is wired. |
| `ROADMAP.md:197,224-225` | Current branch is v0.7; PNACH merge is next/read-only. | Current base is post-v0.7 main; PNACH mutation pipeline is present. |
| `docs/reviews/EMUWIZ_RENAME_AUDIT.md:31` | GitHub repository is intentionally not renamed. | Live repository is `kiehntre/emuwiz`. |
| `docs/GUI_BACKEND_CAPABILITY_MATRIX.md`, `docs/INTEGRATED_GUI_AUDIT.md` | RetroArch/Settings integration absent or partial. | Already identified as dated snapshots by `docs/reviews/CURRENT_MAIN_USER_WORKFLOW_AUDIT.md`; mark superseded, do not silently modernize history. |

## Security / safety debt conclusion

No new archive traversal, symlink escape, token leak, or destructive-operation
vulnerability was established by this audit. Existing code shows deliberate
bounded reads, no-follow checks, trusted roots, destination revalidation,
atomic/no-clobber primitives, backups, journals and rollback. The evidence-backed
debts are:

1. installer/uninstaller ownership assumptions (actionable P0);
2. missing classifier-version enforcement despite a documented safety promise
   (actionable P0);
3. stale public security claims (documentation P0);
4. archive-aware DAT verification not yet designed/preserved (P1 capability and
   false-negative risk, not a current vulnerability);
5. live/integration evidence is weaker than the unit-test count suggests.

RomM token handling is comparatively strong: token files require private mode,
symlinks/public files are refused, values are redacted, redirects are refused,
and tests exercise header-only use. No token-handling follow-up is recommended
without new evidence.

## Top 5 next actions

1. Merge one narrow documentation-truth PR: `SECURITY.md`, README, roadmap,
   changelog/current-main split, adapter matrix, BSFree docs and current repo
   URLs.
2. Add classifier-version mismatch rejection to DAT rename and organisation
   transactions with regression tests.
3. Make install/uninstall ownership-safe, coordinated with #33's desktop/icon
   installation behavior.
4. Triage PRs: finish #33 review, close #32, and explicitly preserve-or-close
   #27/#30.
5. Write and merge a bounded archive-aware DAT verification research/design
   document, then implement one NES/ZIP vertical slice.

## What NOT to touch

- Do not treat #33 as landed or duplicate its desktop/app-icon work.
- Do not implement encrypted AR while the verdict is YELLOW; do not copy or
  translate GPL implementations or unproven constants.
- Do not mass-rename Cargo crates, compatibility paths, persisted fields,
  executable aliases, environment variables, provider/schema IDs, ownership
  markers, journals or historical docs.
- Do not weaken DAT Unknown handling, hash-confidence rules, platform conflicts,
  symlink/trusted-root checks, no-clobber behavior, or explicit confirmation.
- Do not delete branches, worktrees, targets or research until PR preservation
  decisions and a final status check are complete.

## Blockers

- Encrypted AR: authoritative provenance/permission for scheme-specific
  constants and independent known-answer vectors.
- PCSX2 downloadable ordinary-cheat provider: ownership, licence, immutable
  provenance and review policy.
- BSFree Wii real coverage: the current snapshot contains no Wii rows.
- Archive-aware verification: no research artifact was found to review or
  preserve; requirements must be reconstructed.
- Wayland/live filesystem QA: requires suitable real environments and legally
  redistributable fixtures.

## Suggested order for the next 3 PRs

1. **Docs/security/repository truth close-out** — no behavior changes; also mark
   stale audit snapshots as superseded and close the corresponding loose ends.
2. **DAT safety close-out** — enforce classifier version, document B1, and add
   focused rename/organisation mismatch tests. Keep multidisc grouping out of
   this PR.
3. **Installer ownership + desktop integration** — after #33 review settles the
   exact installed assets, add ownership-safe install/upgrade/uninstall and
   adversarial integration tests. If #33 lands first, base this PR on its final
   contract; if it does not, keep desktop work out and harden binaries only.

## Cleanup execution plan — superseded pre-#33 snapshot

> Historical planning snapshot based on `f7c450c`, retained with the original
> audit evidence. PR #33 has since merged. Do not execute the numbered queue in
> this subsection; the refreshed active plan appears at the end of this report.

Plan refresh: `origin/main` was fetched on 2026-08-11 and remains exactly
`f7c450cad3b89207251dd3b2b4747af1f1e01d42`; there are no commits to reconcile
after the audit baseline. PR #33 remains an open draft at
`59e7ec89b6d04ed1c569e3d2cb4a5d92bce2a8db` and is not included in the base or
in any scope below. This plan must be rechecked if #33 or any other PR lands
before execution.

One remote-state change occurred after the audit: draft PR #34 now contains the
research-only file `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md`.
It is remotely preserved but not on `main`; ZIP-member verification and NES
normalization remain unimplemented. PR #34 and encrypted Action Replay PR #30
remain separate workstreams.

### Verified unresolved-item inventory

This table is the dependency map for the queue. “Size” is the expected review
size, not calendar time. A proposed implementation is not authorization to skip
the preceding research/review gate.

| Item and exact current area | Verification on `origin/main` | Dependencies | Risk | Job type | Size | Disposition |
|---|---|---|---|---|---|---|
| Current product/docs truth: `README.md:128,165,185-188,205,210-211`, `CHANGELOG.md:98-99`, `ROADMAP.md:197,224-225,268-274`, `docs/ADAPTER_SUPPORT_MATRIX.md:31,42-44`, `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md:179-181`, `docs/reviews/EMUWIZ_RENAME_AUDIT.md:31` | Claims and old `kiehntre/archivefs` release URLs remain unchanged. Code still has GUI PCSX2 staging/apply and BSFree apply. The repository is `kiehntre/emuwiz`. | None; recheck #33 only for wording about desktop support. | LOW | docs, cleanup | small | Standalone PR; combine only these closely related current-truth corrections. Do not rename legacy artifacts. |
| Public security description: `SECURITY.md:30-55`, `docs/security.md`; network implementations under `identity_source/romm`, `patch_manager/retrieval.rs`, provider clients, and GUI source retrieval | The “only network use” and “preview does not write” claims remain false. Actual token/path controls remain stronger than the prose. | Documentation must describe existing behavior only; no dependency on archive or installer implementation. | MEDIUM | docs, security/safety | small | Standalone docs PR with security review. Keep separate from general prose cleanup so guarantees receive focused review. |
| B1 classifier version: `dat/classification.rs`, `dat/rename_plan/{model,plan}.rs`, `dat/rename_apply/{model,executor,preflight,journal,reconcile,tests}.rs`, `dat/rom_organisation/{model,plan,transaction,tests}.rs`, GUI transaction fixtures | `CLASSIFIER_VERSION` and plan fields remain present, while `RenameTransaction` records only `plan_generation`; build/apply checks generation only. | Journal backward compatibility; both rename and organisation share `RenameTransaction`. | HIGH | implementation, compatibility, security/safety, tests | small | Standalone PR. Reject mismatches before journal creation or mutation; define safe deserialization for older journals. |
| Installer ownership: `install.sh:109-119,170-179`, `tests/test_install.sh`, release-bundle tests/scripts; desktop/icon entries only after #33 | `rm -f`, `cp -f`, and `ln -sf` still operate on fixed names without provenance. Tests preserve an unrelated differently named file but do not protect foreign same-name targets. | Final installed-file set from #33. Any manifest format becomes a compatibility contract. | HIGH | implementation, compatibility, security/safety, tests | medium | Standalone PR after #33. Cover install, upgrade, partial failure, uninstall, foreign files, symlinks, and old installations without manifests. |
| Archive-aware verification research: existing draft #34, file `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md` | Not on `main`, but now safely present in a remote commit. PR body scopes ZIP Stored/Deflate, bounded streaming, fail-closed ambiguity, raw-first NES handling, and untouched archives. | Independent review of safety bounds and exact-match semantics. | MEDIUM | research, security/safety | medium | Review/preserve existing #34 as a standalone research PR; do not mix with code. |
| ZIP-member verification: `dat/sources/audit_run.rs`, `dat/audit.rs`, `dat/index.rs`, `identity_source/hashing.rs`, `safe_read`, reusable ZIP metadata in `inspector.rs`; tests in `dat/sources/tests.rs` and new golden ZIP fixtures | Audit still hashes each outer ZIP as one file. Inspector lists metadata only and does not hash members. | Accepted #34 research; clear member-count/size/ratio/cancellation limits; exact verdict provenance. | HIGH | implementation, security/safety, tests | medium | Standalone reliability PR after research. ZIP only; no 7z/RAR, extraction, NES normalization, rename, or organisation changes. |
| NES header normalization: same DAT audit/index path plus a new normalization/evidence boundary and NES fixtures | No iNES/headerless normalization exists on `main`; raw outer-file hash is the only evidence. #34 proposes raw-hash-first and DAT-authorized normalization, but is unmerged. | Accepted #34 research; ZIP member evidence API if NES-in-ZIP is supported. | HIGH | implementation, compatibility, tests | medium | Standalone PR after the raw/member evidence model is stable. Never silently replace raw evidence or rewrite source bytes. |
| Wrong-platform-folder diagnostic: `DatAuditRequest.platform` and `DatAuditOutcome.platform` in `dat/sources/audit_run.rs`; platform evidence in `platform/detect.rs`, `platform/identity`, `disk_format`; organisation plan and DAT GUI/CLI presentation | Adjacent conflict detection remains, including folder/signature conflicts and DAT/RomM organisation blocks, but DAT audit has no general exact-match-versus-containing-folder finding. | Stable audit evidence/provenance; no dependency on archive-aware verification for ordinary files. | MEDIUM | implementation, tests | medium | Standalone read-only diagnostic PR. It must report, not auto-move; organisation consumption is a later explicit decision. |
| B2 TOSEC/multidisc: `dat/classification.rs:258-280,401-427,535-560`, rename/organisation plan models and GUI DAT presentation | Strict `(Disc|Disk|Part|Side N of M)` tokens still label individual entries `RequiredMultidiscPart`; no group ID, completeness proof, dependency, or atomic action exists. | A reviewed grouping/completeness contract and real redistributable TOSEC-shaped fixtures; B1 should land first so rule versions are enforceable. | HIGH | research first; later implementation, tests | research: small; implementation: large | Research-only PR now. Defer implementation until the contract is accepted; do not combine with ZIP/NES work. |
| Encrypted Action Replay: open draft #30, `docs/research/ENCRYPTED_ACTION_REPLAY_LICENSING_RESEARCH.md`; current Dolphin/BSFree/GameHacking adapters | YELLOW remains current. The research is remotely committed but not on `main`; scheme constants and independent known-answer vectors remain unresolved. | Authoritative permission/provenance and fresh-room review. | HIGH | research, compatibility | medium | Preserve or close #30 deliberately as its own research record. Implementation remains blocked and must not be queued. |
| Rename-compatibility research: open draft #27, `docs/research/EMUWIZ_COMPATIBILITY_RENAME_RESEARCH.md`; landed `docs/reviews/EMUWIZ_RENAME_AUDIT.md` and compatibility code | #27 is remotely preserved but stale against merged #26 and the renamed repository. Some unique migration inventory may still be useful. | Compare unique claims against current `app_dirs.rs`, CLI aliases, serde/schema IDs, journals and merged audit. | MEDIUM | research, compatibility, cleanup | small | Do not merge unchanged. Reduce to unique durable findings or close as superseded. |
| Release naming decision: `scripts/release-common.sh:58`, `docs/RELEASE_ENGINEERING.md`, `.github/workflows/release.yml`, verifier tests and living install docs | Future bundles still use `archivefs-v*`. This is visible but may be an intentional compatibility surface; historical release docs are correct. | #33 release contract; explicit compatibility decision. | MEDIUM | research, docs, compatibility | tiny | Standalone ADR/docs PR after #33. Any later script rename must be separate and accept old artifacts. |
| Filesystem-mutation integration coverage: DAT rename/organisation executor, preflight, journal, rollback and `tests/rename_clean_install.rs` | Extensive tempdir/unit tests remain; real cross-device, permission, symlink-race and interrupted-journal coverage remains thin. | B1 first, so tests exercise the final transaction schema. Needs controlled Linux fixtures/mount capabilities. | MEDIUM | tests, security/safety | medium | Standalone test PR; skip privileged cases clearly when the environment cannot provide them. |
| Compatibility migration coverage: `app_dirs.rs`, config discovery in core/CLI/GUI, database migrations, journal readers, binary aliases, `crates/archivefs-cli/tests/rename_clean_install.rs`, `tests/test_install.sh` | Compatibility paths are implemented, but mixed old/new state, conflicting directories, old journals and downgrade behavior have weak integrated coverage. | Installer ownership contract for installed files; B1 for old DAT journals. | HIGH | tests, compatibility | medium | Standalone tests PR after B1 and installer safety. Tests must preserve rather than rename compatibility identifiers. |
| Archive/platform fixture coverage | ZIP/7z/RAR and platform tests remain mostly synthetic. No member-hash/NES corpus exists. | Add fixtures with the feature that consumes them; document provenance. | MEDIUM | tests, research | small per feature | Combine ZIP/NES fixtures with their own PRs. Defer 7z/RAR live corpus until support is in scope; do not make a miscellaneous fixture PR. |
| GUI beginner live QA: release binary flows in `crates/archivefs-gui`, existing headless page/state tests, manual QA docs | Headless coverage remains strong; Wayland and full first-run/source/DAT/install/undo journeys remain weak. | #33 for final desktop launch/icon behavior; installer safety before destructive install/uninstall journeys. | MEDIUM | tests, docs | small | Standalone executable checklist/test-harness PR after #33 and installer work. Keep it to a few named journeys. |
| PCSX2 ordinary-cheat provider | Install pipeline exists; a licensed, immutable, reviewed provider catalogue still does not. | Provider ownership/licence/provenance. | HIGH | research | unknown | Deferred/blocked. Do not create an implementation PR until provider evidence exists. |
| BSFree Wii real coverage | Adapter and fixture tests exist; shipped snapshot still contains no Wii rows. | Upstream data with acceptable provenance. | LOW | research, tests | tiny when data exists | Deferred. Recheck data at update time; do not add speculative code. |
| New cheat/mod platform adapters and browse-only formats | `CHEATS_MODS_EXPANSION_RESEARCH.md` remains aspirational; no general mod pipeline exists. | Existing adapter/docs/QA close-out. | MEDIUM | research, implementation | large | Deferred. Preserve browse-only status unless a format receives its own reviewed safety contract. |
| GUI information architecture/settings follow-ups | `docs/POST_V0.7_GUI_FOLLOWUPS.md` and live “More settings coming later” wording remain intentional product deferral. | Usage evidence. | LOW | research, docs | medium | Deferred, not cleanup. |
| Deprecated egui compatibility APIs | `docs/DEPENDENCY_SECURITY.md` still records an intentional allowance; no current advisory was found. | Dedicated visual regression capacity. | MEDIUM | implementation, compatibility, tests | medium | Deferred standalone modernization PR; never mix into cleanup/docs work. |
| Catalogue performance | No measured current regression or bottleneck was established. | Real workload profiles. | LOW | research, tests | unknown | Deferred; close as non-actionable until measurements exist. |
| TODO/FIXME/HACK/follow-up comments | A refreshed search still finds no live `TODO`/`FIXME` code debt. “widescreen-hack” is terminology; follow-up comments for implemented GUI work are historical; diagnostic `DEFERRED_CHECKS` are tested user-visible capability statements. | None. | LOW | cleanup | tiny | Close as already solved/stale or intentional. Do not create a comment-only PR unless touched alongside the owning code. |
| User-facing ArchiveFS product labels | No new stale live GUI/CLI label was found. Remaining names are URLs, historical release names, package/crate identifiers, aliases, paths, schema/provider IDs or ownership markers. | Compatibility policy. | HIGH if “cleaned up” incorrectly | compatibility | n/a | Close product-label search as solved; only current repository URLs belong in the docs PR. |

The completed DAT rename/organisation engines, RomM token protections, BSFree
GameCube apply, BSFree Wii adapter plumbing, and GUI follow-up controls need no
standalone cleanup PR. Their only queued work is the specific safety, docs, or
coverage item named above.

### Ordered cleanup queue

The numbering is the recommended merge order after PR #33 has finished review.
Research PRs #30 and #34 may be reviewed in parallel, but implementation must
still respect the dependencies shown here.

#### PR 1 — Reconcile current-main product and repository documentation

- **Exact scope:** correct current behavior for PCSX2 and BSFree; separate
  released v0.7 claims from current `main`; update live repository/release URLs;
  update roadmap state; mark dated audits as superseded without rewriting their
  historical evidence.
- **Likely files:** `README.md`, `CHANGELOG.md`, `ROADMAP.md`,
  `docs/ADAPTER_SUPPORT_MATRIX.md`, `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md`,
  `docs/reviews/EMUWIZ_RENAME_AUDIT.md`; possibly
  `crates/archivefs-core/tests/documentation_claims.rs` if it should lock a
  machine-checkable claim.
- **Dependencies:** recheck final #33 state only for desktop wording; otherwise
  none.
- **Risk / type / size:** **LOW**; docs + cleanup; **small**.
- **Why here:** it gives every later agent an accurate baseline and removes the
  old repository URL immediately.
- **Out of scope:** `SECURITY.md`; release artifact renaming; historical release
  notes; crate/package names; aliases; application behavior.
- **Required tests/review:** link check, documentation-claim tests if changed,
  and reviewer comparison against current GUI/core entry points.
- **Recommended agent:** **DeepSeek**, with a maintainer verifying every behavior
  claim against code.

#### PR 2 — Bring the public security description up to current behavior

- **Exact scope:** document the actual opt-in network clients, credential
  boundaries, cache/source handling, preview-versus-confirmed mutation, journals
  and rollback. State only test-backed guarantees.
- **Likely files:** `SECURITY.md`, `docs/security.md`; documentation-claim tests
  only if a stable assertion is appropriate.
- **Dependencies:** none; describe current behavior, not planned ZIP or installer
  fixes.
- **Risk / type / size:** **MEDIUM**; docs + security/safety; **small**.
- **Why here:** a correct public boundary should precede more reliability work,
  but focused review is easier when separated from PR 1.
- **Out of scope:** code changes, new guarantees, vulnerability claims,
  installer ownership fixes, archive-member design.
- **Required tests/review:** security-focused review against RomM, retrieval,
  provider, shared transaction and rollback code; link/format checks.
- **Recommended agent:** **Codex**.

#### PR 3 — Preserve and approve archive-aware DAT verification research

- **Exact scope:** review existing draft #34 and land or otherwise durably
  preserve `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md` with base
  commit, limits, provenance, ambiguity behavior and explicit non-implementation
  status.
- **Likely files:** the single research file already in #34.
- **Dependencies:** none; it must remain independent of PR #30 and #33.
- **Risk / type / size:** **MEDIUM**; research + security/safety; **medium**.
- **Why here:** ZIP/NES work must not start from an unreviewed or temporary
  design.
- **Out of scope:** Rust changes, extraction, 7z/RAR, ZIP-member hashing, NES
  normalization, rename/organisation changes.
- **Required tests/review:** no runtime tests; two-person technical review of
  bounds, cancellation, ambiguity, hash provenance and upstream citations.
- **Recommended agent:** **Claude Code**.

#### PR 4 — Preserve or formally close encrypted Action Replay YELLOW research

- **Exact scope:** review existing draft #30, keep the YELLOW verdict explicit,
  and either land a durable non-legal-advice research record or close with an
  equally durable preservation reference.
- **Likely files:**
  `docs/research/ENCRYPTED_ACTION_REPLAY_LICENSING_RESEARCH.md` in #30 only.
- **Dependencies:** authoritative source/licence review; no dependency on
  archive-aware verification.
- **Risk / type / size:** **HIGH**; research + compatibility/licensing;
  **medium**.
- **Why here:** it closes the risk of the only research copy living outside
  `main` while preventing premature implementation.
- **Out of scope:** decryptor code, constants copied or translated from GPL
  implementations, speculative vectors, adapter changes.
- **Required tests/review:** source-by-source provenance and licence review; no
  code tests. The result must still say implementation is blocked if provenance
  remains unresolved.
- **Recommended agent:** **Claude Code**.

#### PR 5 — Enforce DAT classifier versions at transaction boundaries

- **Exact scope:** carry the reviewed classifier version into rename and
  organisation transactions, reject mismatches before journal/mutation, and
  define conservative behavior for older journals missing the field.
- **Likely files:** `crates/archivefs-core/src/dat/classification.rs`,
  `rename_plan/model.rs`, `rename_apply/{model,executor,preflight,journal,reconcile,tests}.rs`,
  `rom_organisation/{model,transaction,tests}.rs`, and affected GUI fixtures.
- **Dependencies:** PRs 1-2 are documentation-only; no code dependency. Land
  before B2 or transaction integration tests.
- **Risk / type / size:** **HIGH**; implementation + compatibility +
  security/safety + tests; **small**.
- **Why here:** this is the narrowest missing safety promise and stabilizes the
  transaction schema for later work.
- **Out of scope:** classifier rule changes, B2 grouping, archive hashing,
  platform diagnostics, generation semantics.
- **Required tests/review:** mismatches at build and apply for both rename and
  organisation; matching version success; old/missing/unknown journal field;
  no journal or filesystem mutation on refusal; full DAT transaction suite.
- **Recommended agent:** **Codex**.

#### PR 6 — Make install, upgrade and uninstall ownership-safe

- **Exact scope:** protect foreign same-name files, define owned-entry evidence
  or a manifest, safely handle upgrades and installations made before that
  evidence existed, and remove only proven-owned entries.
- **Likely files:** `install.sh`, `tests/test_install.sh`, release verification
  scripts/tests, and the final desktop/icon install files introduced by #33.
- **Dependencies:** #33 must be merged or definitively closed; its final installed
  asset set is the contract. Coordinate any manifest name/location with legacy
  installs.
- **Risk / type / size:** **HIGH**; implementation + compatibility +
  security/safety + tests; **medium**.
- **Why here:** it closes the most concrete destructive edge case before new
  installation surfaces accumulate.
- **Out of scope:** DAT/cheat code, release archive renaming, config/data
  migration, removing legacy executable aliases.
- **Required tests/review:** foreign regular files and symlinks for every fixed
  name; repeat install; upgrade; old unmanifested installation; partial failure;
  uninstall; custom/symlinked prefixes; desktop/icon ownership where applicable;
  shellcheck/portable-shell review.
- **Recommended agent:** **Claude Code**.

#### PR 7 — Add a read-only wrong-platform-folder DAT diagnostic

- **Exact scope:** compare authoritative exact DAT platform evidence with the
  containing/source-folder platform and emit a separate, provenance-rich
  diagnostic without changing the underlying DAT verdict.
- **Likely files:** `dat/sources/audit_run.rs`, `dat/audit.rs` or a narrowly
  scoped diagnostic model, platform identity helpers, CLI DAT reporting,
  `crates/archivefs-gui/src/dat_sources_page.rs`, and their tests.
- **Dependencies:** use current platform conflict semantics; no automatic
  organisation dependency.
- **Risk / type / size:** **MEDIUM**; implementation + tests; **medium**.
- **Why here:** it improves read-only truth before archive normalization creates
  more kinds of exact evidence.
- **Out of scope:** moving/renaming files, changing platform assignments,
  guessing from weak filename evidence, ZIP/NES support.
- **Required tests/review:** exact same-platform, exact different-platform,
  unknown folder, manual assignment conflict, multiple-DAT ambiguity, no-match,
  symlink/path display, and proof that no filesystem/database mutation occurs.
- **Recommended agent:** **either** Codex or Claude Code.

#### PR 8 — Verify bounded ZIP members against DAT evidence

- **Exact scope:** implement the reviewed ZIP-only P0 from PR #34: stream
  supported members read-only, produce explicit member provenance, apply
  member/count/size/ratio/cancellation bounds, and fail closed on ambiguous or
  unsupported archives.
- **Likely files:** `dat/sources/audit_run.rs`, `dat/audit.rs`, `dat/index.rs`,
  `identity_source/hashing.rs`, `safe_read`, selected reusable pieces of
  `inspector.rs`, `dat/sources/tests.rs`, and redistributable ZIP fixtures.
- **Dependencies:** accepted PR #34. Reuse rather than fork the existing safe-read
  and audit-verdict models.
- **Risk / type / size:** **HIGH**; implementation + security/safety + tests;
  **medium**.
- **Why here:** the research and basic audit/platform cleanup are settled first;
  this is the first substantial new reliability capability.
- **Out of scope:** extraction, writing, nested archives, 7z/RAR, NES header
  normalization, multidisc grouping, automatic rename/organisation.
- **Required tests/review:** Stored and Deflate; directories; duplicate/case
  names; encrypted/unsupported methods; CRC/read errors; nested archive;
  truncated/over-limit/bomb-shaped input; cancellation; symlinked archive;
  single and multiple exact matches; raw outer hash preserved; zero writes.
- **Recommended agent:** **Claude Code**.

#### PR 9 — Add raw-first NES DAT normalization

- **Exact scope:** retain raw-file/member evidence, then try only the reviewed
  iNES/headerless transformation authorized by DAT metadata; promote only an
  exact normalized match with explicit provenance.
- **Likely files:** a new narrow DAT normalization module, `dat/hash.rs`,
  `dat/index.rs`, `dat/audit.rs`, `dat/sources/audit_run.rs`, and NES golden
  fixtures/tests. ZIP integration should consume PR 8's member evidence API,
  not duplicate it.
- **Dependencies:** accepted PR #34; PR 8 if NES-in-ZIP is included. Raw loose
  NES may be implemented first only if the interface is demonstrably reusable.
- **Risk / type / size:** **HIGH**; implementation + compatibility + tests;
  **medium**.
- **Why here:** normalization is more trustworthy after evidence provenance and
  archive bounds are established.
- **Out of scope:** modifying ROM bytes, heuristic header synthesis without DAT
  authorization, other console transforms, archive extraction, B2 grouping.
- **Required tests/review:** raw exact wins; valid header strip/add cases allowed
  by metadata; malformed/trainer/size mismatch; ambiguous normalized results;
  raw and normalized provenance; loose and (if enabled) ZIP member; cancellation
  and no writes.
- **Recommended agent:** **Claude Code**.

#### PR 10 — Specify the B2 multidisc grouping and completeness contract

- **Exact scope:** research and document trusted set identity, token parsing,
  completeness/missing-part states, clone/variant separation, review UX and
  whether group actions must be atomic.
- **Likely files:** a new `docs/research/` or `docs/design/` B2 document, with
  citations and fixture inventory; no production code.
- **Dependencies:** PR 5 so future classifier changes have enforceable version
  semantics. Use real, redistributable TOSEC-shaped evidence.
- **Risk / type / size:** **HIGH**; research; **small**.
- **Why here:** it deliberately follows the smaller DAT safety and evidence
  work and prevents the current token label from becoming an accidental group
  identity API.
- **Out of scope:** classifier/model changes, group selection/apply, ZIP/NES,
  looser “Disc” word matching.
- **Required tests/review:** no runtime tests; adversarial design review covering
  missing/duplicate parts, regions, revisions, sides, compilations, same-title
  collisions and incomplete sets.
- **Recommended agent:** **Claude Code**.

The B2 implementation is **deferred**, expected **large** and **HIGH** risk. It
should become its own later PR only after PR 10 is accepted, with group-level
classification/plan/GUI tests and atomicity decisions. It must not be smuggled
into PR 10 or an archive PR.

#### PR 11 — Add live DAT mutation and compatibility regression coverage

- **Exact scope:** add a bounded Linux integration suite for real rename and
  organisation mutations plus mixed EmuWiz/ArchiveFS state and old journal
  compatibility. Prefer capability-detected tests over privileged assumptions.
- **Likely files:** core DAT integration tests, CLI
  `tests/rename_clean_install.rs`, compatibility fixtures for `app_dirs.rs` and
  journals, and test runner/CI configuration if needed.
- **Dependencies:** PR 5's final journal schema and PR 6's installer contract.
- **Risk / type / size:** **HIGH**; tests + compatibility + security/safety;
  **medium**.
- **Why here:** tests should encode the settled contracts rather than chase
  schemas changing in adjacent PRs.
- **Out of scope:** changing mutation behavior merely to make tests pass;
  privileged CI requirements; deleting compatibility paths; GUI automation.
- **Required tests/review:** no-clobber on the real filesystem; permissions;
  cross-device refusal where available; symlink swaps; interrupted journals;
  rollback; both legacy/current directories; conflicts; old/missing fields;
  aliases and downgrade/read-only behavior.
- **Recommended agent:** **Codex**.

#### PR 12 — Record the future release-artifact naming contract

- **Exact scope:** decide and document whether future bundles remain
  `archivefs-v*` for compatibility or become `emuwiz-v*`, including acceptance
  of historical names and a migration rule.
- **Likely files:** a small ADR or `docs/RELEASE_ENGINEERING.md` and
  `docs/release-checklist.md`; no script changes in this PR.
- **Dependencies:** #33 and PR 6 define the final release/install payload.
- **Risk / type / size:** **MEDIUM**; research + docs + compatibility; **tiny**.
- **Why here:** the decision follows settled desktop/install behavior and stays
  distinct from changing deterministic release machinery.
- **Out of scope:** renaming historical artifacts/docs, changing
  `scripts/release-common.sh`, removing old verifier support, package/crate
  renames.
- **Required tests/review:** compatibility and release-maintainer review; no
  runtime tests. If a rename is chosen, create a separate implementation PR
  with positive/negative deterministic-verifier tests.
- **Recommended agent:** **DeepSeek** for the inventory/draft, with maintainer
  ownership of the final compatibility decision.

#### PR 13 — Codify a small GUI beginner release-journey checklist

- **Exact scope:** define and, where practical, automate a few release-binary
  journeys: first run, source scan, DAT verify, supported cheat install and undo,
  cancellation/error recovery, and desktop launch on X11/Wayland.
- **Likely files:** manual QA documentation and a narrowly scoped GUI smoke-test
  harness; existing page/state tests only where a missing seam is exposed.
- **Dependencies:** #33 desktop behavior and PR 6 installer ownership behavior.
- **Risk / type / size:** **MEDIUM**; tests + docs; **small**.
- **Why here:** it validates the cleanup contracts through beginner workflows
  without holding safety fixes behind GUI automation.
- **Out of scope:** navigation redesign, new settings, screenshot-wide visual
  rewrites, feature implementation, encrypted AR, unsupported adapters.
- **Required tests/review:** recorded X11 and Wayland results, keyboard/mouse
  journey, cancellation, rollback and clear environment/skip criteria.
- **Recommended agent:** **either**.

### Deferred or closed queue entries

- **B2 implementation:** deferred until PR 10; separate large PR.
- **Encrypted AR implementation:** blocked after PR 4 unless permission,
  provenance and independent vectors turn YELLOW into an affirmative reviewed
  result.
- **PCSX2 ordinary provider:** blocked on licensed, immutable provider evidence.
- **BSFree Wii expansion:** deferred until upstream data actually contains
  admissible Wii rows.
- **7z/RAR member verification:** deferred; PR 8 is ZIP-only.
- **New cheat/mod adapters and browse-only formats:** deferred until current
  adapter/docs/QA close-out. Browse-only is intentional, not an incomplete
  installer.
- **egui modernization, GUI redesign and performance optimization:** deferred
  standalone work, each requiring its own evidence/tests.
- **TODO/FIXME/follow-up sweep:** closed as already solved, historical, ordinary
  prose or intentionally tested deferment. No sweep PR.
- **General ArchiveFS label cleanup:** closed as solved. Only current URLs are
  stale; compatibility/history identifiers below remain protected.
- **PR #27:** close as superseded unless review finds unique current migration
  evidence; if so, reduce it to that evidence before preserving it.
- **PR #32:** close unless a maintainer explicitly chooses a separate
  asset-reencoding/provenance review. Do not combine it with #33.

### Safe manual cleanup

These are eventual maintainer actions, not steps performed by this audit. Run a
fresh clean/merged/PR comparison immediately before deleting or closing
anything.

- **Close PR #32** after confirming #33 did not adopt any of its reencoded
  assets. The bot PR is not an application-code prerequisite.
- **Close or reduce PR #27** after comparing its unique findings with merged
  #26 and `docs/reviews/EMUWIZ_RENAME_AUDIT.md`.
- **Preserve/review PRs #30 and #34** rather than abandoning their only durable
  research commits. They must remain separate.
- **Prune two missing worktree registrations:** the detached PR13 scratch path
  under `/tmp/claude-1000/.../archivefs_pr13/worktree` and
  `/tmp/opencode/archivefs-live-ux`. Both are recorded as prunable because their
  gitdirs no longer exist.
- **Remove clean merged/superseded worktrees only after branch comparison:** the
  registered `/home/davedap/archivefs-*` feature/review worktrees plus
  `/home/davedap/emuwiz-bsfree-wii` and `/home/davedap/emuwiz-rename` are prime
  candidates. Keep `/home/davedap/archivefs` (active #33) and
  `/tmp/opencode/emuwiz-ar-licensing` (#30) active for now.
- **Reclaim duplicate build output after worktree decisions:** the current
  `/home/davedap/archivefs/target` is about 80 GiB and the merged BSFree Wii
  worktree target is about 28 GiB. The latter is the clearest candidate, but
  confirm no process/worktree needs it first. Build output is reproducible.
- **Delete merged remote branches in batches:** 49 local remote-tracking
  feature/research/
  integration/review branches (excluding `origin/main` and the symbolic HEAD)
  are exact ancestors of `origin/main` after the refresh. Use the explicit list
  from `git branch -r --merged origin/main`, protect every open-PR head, and
  delete only after owner confirmation.
- **Compare 21 non-ancestor remote branches before deletion:** many appear
  squash-merged or superseded, but ancestry alone cannot prove that. In
  particular keep `feature/emuwiz-linux-desktop-icon`, `imgbot`,
  `research/emuwiz-compatibility-rename`, and
  `research/encrypted-action-replay-licensing` until their PRs are resolved.
  PR #34's head was verified through live GitHub metadata but was not fetched
  into this local remote-tracking ref set; protect it independently too.
- **Mark dated audits as superseded rather than deleting them.** Historical
  release notes, review snapshots, and research with base commits explain why
  current compatibility surfaces exist.

### Do not touch

The following can look stale in searches but are compatibility, ownership or
history. Future cleanup agents must not rename/delete them without a dedicated
migration design and backward-compatibility tests.

- Cargo package/crate/library names such as `archivefs-core`, `archivefs-cli`
  and `archivefs-gui`, plus their target binary history.
- Legacy executable aliases `archivefs-cli`, `archivefs-gui` and the current
  `emuwiz-gui` alias; #33 also explicitly preserves them.
- Legacy config/data/cache lookup including `~/.config/archivefs`, old database
  locations, cache locations, backup paths and recovery reachability.
- `ARCHIVEFS_*` environment variables and compatibility fallbacks.
- Serialized/persisted field names, database columns/migrations, serde keys,
  operation/provider/source IDs, schema identifiers and journal fields.
- Managed block/section markers or ownership sentinels containing ArchiveFS.
- Existing journals, backups, transaction IDs and rollback history.
- Historical release filenames and historical docs referring to
  `archivefs-v*`; living docs may explain them but must not rewrite history.
- Old release verification support for legacy artifact/payload names, even if a
  future EmuWiz-first bundle name is approved.
- Historical audits/research/release notes whose old claims are explicitly
  marked with their base commit or superseded status.
- Conservative DAT `Unknown`, confidence and ambiguity behavior; exact-match
  provenance; no-clobber; trusted-root/symlink checks; explicit confirmation;
  cancellation; journals and rollback.
- Browse-only cheat/mod formats and blocked adapters until their own reviewed
  write contract exists.
- Any file, branch, commit or asset belonging to PR #33 while it is being fixed
  and re-reviewed.

### Final recommendations

- **Recommended first cleanup PR after #33:** PR 1, “Reconcile current-main
  product and repository documentation.” It is low risk, immediately removes
  misleading user/contributor guidance, and creates the truthful base for all
  later reviews.
- **Recommended first substantial feature/reliability PR after cleanup:** PR 8,
  bounded ZIP-member DAT verification, but only after research PR #34 is
  reviewed and preserved. It addresses a real verification false-negative class
  while remaining read-only and tightly scoped.
- **Top three items that need Claude Code rather than a cheaper agent:** (1)
  review/preservation of archive-aware research and the bounded ZIP-member
  implementation contract; (2) encrypted Action Replay licensing/provenance
  review; (3) installer ownership-safe upgrade/uninstall design. B2 multidisc
  research should also receive Claude review before any implementation.
- **Top three safe jobs for DeepSeek:** (1) PR 1's code-evidence-backed docs/URL
  reconciliation; (2) draft the release-artifact naming inventory/ADR for PR 12
  without changing scripts; (3) prepare the GUI manual journey matrix for PR 13
  without changing application behavior. Maintainers must still verify claims
  and compatibility decisions.
- **Item Codex is particularly suitable to implement:** PR 5, B1 classifier
  version enforcement. It is a narrow Rust transaction/journal invariant with
  crisp negative tests across two shared apply paths; Codex is also well suited
  to PR 11's compatibility and mutation regression suite after the schemas
  settle.

## Cleanup execution plan — refreshed after PR #33

Refresh date: 2026-08-11. The fetch attempt encountered a transient DNS failure,
but the existing `origin/main`, local `main`, the supplied target, and the live
merged-PR record all agree on
`7c8d6ea1891d4bd32bcdb0716ff7d998ec08ed83`. That commit is the merge of PR
#33. The complete `f7c450c..7c8d6ea` diff was inspected; it changes only the
desktop/icon/install/release areas listed below, so unrelated DAT, security-doc,
Cheats & Mods, roadmap and audit findings remain current unless explicitly
reclassified here.

Live open PRs are #27 (compatibility-rename research), #30 (encrypted Action
Replay licensing research), #32 (ImgBot), and #34 (archive-aware DAT
verification research). PRs #30 and #34 are still open drafts and unmerged.
No other open research PR was found.

### What PR #33 resolved

The following are **DONE** on current `main`:

- Stable Linux application ID `io.github.kiehntre.emuwiz`, embedded approved
  256px GUI icon, and matching `StartupWMClass` in
  `crates/archivefs-gui/src/main.rs` and
  `assets/linux/io.github.kiehntre.emuwiz.desktop.in`.
- Canonical production artwork under `assets/branding/`; the old
  `docs/assets/branding/README.md` is now a compatibility link/documentation
  bridge, not a stale duplicate asset source.
- Desktop launcher and hicolor 32/64/128/256/512 icon installation under the
  effective absolute `XDG_DATA_HOME`, including relative-XDG fallback and
  safely quoted absolute `Exec` handling.
- Release packaging and verification of the exact desktop template and approved
  icons, including member type/mode/path validation, PNG validation,
  substitution/malformed/duplicate negative tests, CI artifact checks and
  deterministic packaging.
- The release artifact name `archivefs-v*` is now an explicit, tested canonical
  compatibility contract in `docs/RELEASE_ENGINEERING.md`,
  `scripts/release-common.sh` and the verifier. It is no longer an undecided
  branding loose end.
- PR #33 review/merge itself. Its feature branch is no longer active work and
  can enter normal merged-branch cleanup after a fresh status check.

PR #33 also added strong symlink no-follow behavior for launcher/icon replacement
and substantially expanded installer tests. It did **not** prove ownership of a
same-name destination: `install.sh:96-107,146-155,214-224,256-285` still
overwrites/removes fixed binary, desktop and icon paths. Installer ownership
therefore remains active and now covers the landed desktop assets too.

### Refreshed ranked cleanup board

#### P0 — before the next major feature

| Item | Current status on `7c8d6ea` | Next action | Risk / type / size |
|---|---|---|---|
| Current product/docs truth: `README.md:73,112-128,162-188,205-211`, `CHANGELOG.md:98-99`, `ROADMAP.md:197,224-225,268-274`, `docs/ADAPTER_SUPPORT_MATRIX.md`, `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md`, `docs/reviews/EMUWIZ_RENAME_AUDIT.md:31`, `docs/RELEASE_ENGINEERING.md:214` | Still stale. PR #33 changed only the README logo path and release/desktop docs; old repo URLs, PCSX2/BSFree behavior claims, roadmap state and `archivefs-cli doctor` living guidance remain. | One current-main documentation reconciliation PR. Preserve historical release notes and compatibility examples. | **LOW**; docs/cleanup; small |
| `SECURITY.md:31-58`, `docs/security.md` | Still says PCSX2 metadata is the only network use and mutation is future/read-only. No security code changed in #33. | Rewrite the public boundary around actual opt-in network clients, confirmed mutation, tokens, caches, journals and rollback. | **MEDIUM**; docs/security; small |
| Games-only B1 classifier version in `dat/classification.rs`, rename plan/apply and organisation transaction | Unchanged: plans record `CLASSIFIER_VERSION`; `RenameTransaction` and apply gates still enforce generation only. | Carry/enforce the reviewed version with conservative old-journal handling and no-mutation mismatch tests. | **HIGH**; implementation/compatibility/safety/tests; small |
| Installer ownership in `install.sh` and `tests/test_install.sh` | Still unresolved and broader after #33. `remove_owned_path` is named as ownership-aware but checks only object kind/existence; foreign same-name files/symlinks can be overwritten or deleted. | Add manifest/content/symlink ownership proof, refuse or back up foreign targets, and support pre-manifest upgrades/uninstall. | **HIGH**; implementation/compatibility/safety/tests; medium |
| Open-PR/research close-out (#27/#30/#32/#34) | Four open PRs remain. #34 and #30 are preserved remotely but not on `main`; #27 is stale; #32 is superseded by approved landed assets. | Review/preserve #34; preserve-or-close #30 with YELLOW intact; reduce/close #27; close #32 unless separately justified. | **MEDIUM** overall; review/research/cleanup |

#### P1 — soon

| Item | Current status on `7c8d6ea` | Next action | Risk / type / size |
|---|---|---|---|
| Archive-aware DAT research, draft #34 | Preserved in remote commit `91109cb`, unmerged; no production code. | Technical review and durable preservation before implementation. | **MEDIUM**; research/safety; medium |
| ZIP-member DAT verification | Audit still hashes outer files; #33 did not touch DAT code. | After #34, implement bounded ZIP Stored/Deflate member evidence only. | **HIGH**; implementation/safety/tests; medium |
| NES normalization | No raw-first iNES/headerless normalization exists. | Separate PR after the evidence model is stable; never rewrite source bytes. | **HIGH**; implementation/compatibility/tests; medium |
| Wrong-platform-folder DAT diagnostic | Adjacent platform conflicts exist, but no general exact-DAT-versus-folder finding. | Add a read-only, provenance-rich diagnostic; never auto-move. | **MEDIUM**; implementation/tests; medium |
| Games-only B2 TOSEC/multidisc | Individual strict tokens are retained, but grouping/completeness/atomicity remain absent. | Research/design PR after B1; implementation remains deferred. | **HIGH**; research first; small research / large implementation |
| Filesystem mutation and compatibility integration tests | PR #33 improved installer/desktop fixtures, not DAT cross-device, interruption, old-journal or mixed-state coverage. | Add bounded Linux integration tests after B1 and installer contracts settle. | **HIGH**; tests/compatibility/safety; medium |
| GUI beginner live QA | PR #33 covers deterministic assets, desktop-file validation, app ID, icon and an isolated X11 launch. Wayland and full first-run/source/DAT/install/undo journeys remain thin. | Narrow the old QA proposal to Wayland and end-to-end beginner journeys; do not retest solved asset byte identity. | **MEDIUM**; tests/docs; small |

#### P2 — later

- Encrypted Action Replay implementation remains **blocked** by #30's YELLOW
  provenance/licensing result and missing independent known-answer vectors.
- PCSX2 downloadable ordinary-cheat content remains blocked on a licensed,
  immutable, reviewed provider catalogue.
- BSFree Wii remains technically implemented but dormant because the current
  snapshot contains no Wii rows.
- B2 multidisc implementation remains deferred until a grouping/completeness
  design is accepted.
- 7z/RAR member verification, new cheat/mod adapters, browse-only-format write
  support, egui modernization, GUI information-architecture redesign and
  catalogue performance work remain separate evidence-led workstreams.

#### DONE / close-out

- PR #33's Linux desktop/icon/application-ID/install-payload/release-verifier
  work is landed.
- Approved EmuWiz branding and Games-only P0 remain landed.
- `archivefs-v*` release artifact naming is an intentional enforced
  compatibility surface, not a pending rename.
- The refreshed `TODO`/`FIXME`/`HACK` search found only historical audit prose;
  there is no new live code marker to queue.
- No genuinely stale live GUI/CLI `ArchiveFS` product label was found. Current
  repository URLs and one living release command are docs drift; other hits are
  compatibility/history/ownership/package identifiers.
- RomM token handling and the completed DAT rename/organisation engines have no
  new #33-related loose end.

### Revised active cleanup queue

There are **12 active reviewable PR-sized jobs**, down from 13. The old release
artifact naming ADR (previous PR 12) is obsolete because #33 deliberately
codified and tests `archivefs-v*`. The old combined “installer ownership +
desktop integration” proposal is obsolete in that form: desktop integration is
done, while the narrower ownership-safety job remains.

#### PR 1 — Reconcile current-main product and repository documentation

- **Scope/files:** correct living behavior and URLs in `README.md`,
  `CHANGELOG.md`, `ROADMAP.md`, `docs/ADAPTER_SUPPORT_MATRIX.md`,
  `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md`,
  `docs/reviews/EMUWIZ_RENAME_AUDIT.md`, and the living CLI command in
  `docs/RELEASE_ENGINEERING.md`.
- **Dependencies/risk:** none; **LOW**, docs/cleanup, small.
- **Out of scope:** security policy, historical release notes,
  `archivefs-v*`, aliases, crate/package names and behavior changes.
- **Tests/review:** link check, documentation-claim tests where appropriate,
  code-evidence review.
- **Agent:** **DeepSeek** with maintainer verification.
- **Why first:** every later agent needs truthful current-main guidance.

#### PR 2 — Bring the public security description up to current behavior

- **Scope/files:** `SECURITY.md`, `docs/security.md`; actual opt-in network,
  credential, cache, mutation, journal and rollback boundaries.
- **Dependencies/risk:** none; **MEDIUM**, docs/security, small.
- **Out of scope:** new guarantees, code fixes, speculative vulnerabilities.
- **Tests/review:** security review against RomM/retrieval/provider and shared
  transaction code; documentation checks.
- **Agent:** **Codex**.
- **Why here:** the incorrect public safety boundary is a P0 truth defect.

#### PR 3 — Enforce Games-only B1 classifier versions

- **Scope/files:** `dat/classification.rs`, rename plan/apply model, executor,
  preflight, journal/reconcile/tests, organisation transaction/tests and affected
  GUI fixtures.
- **Dependencies/risk:** old-journal compatibility; **HIGH**,
  implementation/compatibility/safety/tests, small.
- **Out of scope:** B2 rules, grouping, ZIP/NES and generation redesign.
- **Tests/review:** build/apply mismatch for rename and organisation; matching
  success; missing/old journal fields; zero journal/filesystem mutation on
  refusal.
- **Agent:** **Codex**.
- **Why here:** it is the narrowest missing mutation invariant and stabilizes the
  journal schema for later tests.

#### PR 4 — Make all installed entries ownership-safe

- **Scope/files:** `install.sh`, `tests/test_install.sh`, release installer
  verification as needed; binaries, three aliases, desktop entry and five icons.
- **Dependencies/risk:** #33 is now landed, so the installed asset set is stable;
  **HIGH**, implementation/compatibility/safety/tests, medium.
- **Out of scope:** removing aliases, renaming artifacts, config/data migration,
  DAT or cheat code.
- **Tests/review:** foreign same-name regular files and symlinks at every class of
  destination; fresh/pre-manifest upgrade; repeat install; partial failure;
  custom XDG/prefix; uninstall and manifest tampering; portable-shell review.
- **Agent:** **Claude Code**.
- **Why here:** #33 removed the dependency blocker and expanded the potential
  overwrite/delete set, making this the highest-risk remaining mutation cleanup.

#### PR 5 — Review and preserve archive-aware DAT research (#34)

- **Scope/files:** existing
  `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md` only.
- **Dependencies/risk:** none; **MEDIUM**, research/safety, medium.
- **Out of scope:** Rust, extraction, ZIP hashing, NES normalization, 7z/RAR.
- **Tests/review:** two-person review of limits, cancellation, ambiguity, raw
  evidence, provenance and citations.
- **Agent:** **Claude Code**.
- **Why here:** it is the gate for the next reliability feature and must not
  remain only in an open draft.

#### PR 6 — Preserve or formally close encrypted AR YELLOW research (#30)

- **Scope/files:** existing
  `docs/research/ENCRYPTED_ACTION_REPLAY_LICENSING_RESEARCH.md` only.
- **Dependencies/risk:** authoritative provenance/licence review; **HIGH**,
  research/licensing, medium.
- **Out of scope:** decryptor code, copied/translated GPL logic or constants,
  speculative vectors.
- **Tests/review:** source-by-source provenance review; retain the blocked/YELLOW
  result unless new authoritative evidence changes it.
- **Agent:** **Claude Code**.
- **Why here:** durable preservation closes research risk without conflating it
  with archive verification or authorizing implementation.

#### PR 7 — Add a read-only wrong-platform-folder DAT diagnostic

- **Scope/files:** `dat/sources/audit_run.rs`, audit/diagnostic model, platform
  identity helpers, CLI and GUI DAT reporting/tests.
- **Dependencies/risk:** current exact-evidence semantics; **MEDIUM**,
  implementation/tests, medium.
- **Out of scope:** moves, renames, platform reassignment, weak guessing,
  ZIP/NES.
- **Tests/review:** same/different/unknown folder, manual conflict, ambiguity,
  no-match and zero mutation.
- **Agent:** **either**.
- **Why here:** finish read-only placement truth before adding normalized member
  evidence.

#### PR 8 — Verify bounded ZIP members against DAT evidence

- **Scope/files:** DAT audit/index/hashing/safe-read path, reusable ZIP reading
  from `inspector.rs`, sources tests and redistributable ZIP fixtures.
- **Dependencies/risk:** accepted #34; **HIGH**,
  implementation/safety/tests, medium.
- **Out of scope:** extraction, nested archives, 7z/RAR, NES normalization,
  rename/organisation and B2.
- **Tests/review:** Stored/Deflate, duplicate/case names, unsupported/encrypted
  methods, corruption, bounds/bomb-shaped input, cancellation, symlinks,
  ambiguous matches, raw outer provenance and zero writes.
- **Agent:** **Claude Code**.
- **Why here:** first substantial reliability feature after cleanup/research.

#### PR 9 — Add raw-first NES DAT normalization

- **Scope/files:** narrow DAT normalization/evidence module, hash/index/audit run
  and NES golden fixtures; consume PR 8 member evidence rather than duplicating
  it when supporting NES-in-ZIP.
- **Dependencies/risk:** accepted #34 and PR 8 for archive members; **HIGH**,
  implementation/compatibility/tests, medium.
- **Out of scope:** source rewriting, heuristic unauthorized headers, other
  consoles and B2.
- **Tests/review:** raw exact precedence, authorized add/strip, trainer/size and
  malformed cases, ambiguity, provenance, loose/member inputs, cancellation and
  zero writes.
- **Agent:** **Claude Code**.
- **Why here:** normalization follows a stable raw/member evidence model.

#### PR 10 — Specify the B2 multidisc grouping/completeness contract

- **Scope/files:** new research/design document and fixture inventory only.
- **Dependencies/risk:** PR 3 B1 enforcement; **HIGH**, research, small.
- **Out of scope:** production models, GUI/apply, loose “Disc” matching,
  ZIP/NES.
- **Tests/review:** adversarial design review for missing/duplicate parts,
  sides, regions, revisions, compilations, same-title collisions and atomicity.
- **Agent:** **Claude Code**.
- **Why here:** research prevents the current per-entry token from becoming an
  accidental group identity contract. Implementation remains a later large PR.

#### PR 11 — Add live DAT mutation and compatibility regression coverage

- **Scope/files:** DAT integration tests, CLI rename clean-install tests,
  `app_dirs.rs`/journal compatibility fixtures and capability-detected CI hooks.
- **Dependencies/risk:** PR 3 journal schema and PR 4 installer ownership
  contract; **HIGH**, tests/compatibility/safety, medium.
- **Out of scope:** behavior changes to satisfy tests, privileged mandatory CI,
  deleting compatibility paths and GUI automation.
- **Tests/review:** real no-clobber, permissions, cross-device refusal where
  available, symlink swaps, interruption/recovery/rollback, old fields and mixed
  old/new directories.
- **Agent:** **Codex**.
- **Why here:** encode settled contracts after their schemas stop moving.

#### PR 12 — Codify remaining beginner release journeys

- **Scope/files:** narrow manual QA record and practical smoke harness for first
  run, source scan, DAT verify, supported install/undo and cancellation/error
  recovery on release binaries, especially Wayland.
- **Dependencies/risk:** #33 desktop work is landed; PR 4 for ownership-safe
  install/uninstall journeys; **MEDIUM**, tests/docs, small.
- **Out of scope:** retesting approved icon byte identity, X11 app-ID work already
  covered by #33, redesign, features or unsupported adapters.
- **Tests/review:** recorded X11/Wayland environment, keyboard/mouse path,
  rollback and explicit skip criteria.
- **Agent:** **either**.
- **Why here:** validates the final cleanup contracts without delaying safety
  fixes behind GUI automation.

### Safe manual cleanup — refreshed

Do not perform any item without a fresh clean/merged/open-PR check.

- Close #32 unless a maintainer explicitly wants a new provenance review of
  reencoded assets. #33 landed and verifies the approved bytes; #32 is now more
  clearly superseded.
- Reduce #27 to unique current compatibility evidence or close it as superseded
  by #26, the merged rename audit and the actual repository rename.
- Review/preserve #30 and #34 separately. Their remote commits are durable, but
  neither research record is on `main`.
- The former PR #33 branch `feature/emuwiz-linux-desktop-icon` is merged and no
  longer owns the primary worktree. It is now a normal branch deletion
  candidate after confirming no follow-up commit exists.
- Prune the two missing registrations under the PR13 `/tmp/claude-1000/...`
  path and `/tmp/opencode/archivefs-live-ux`.
- Most registered `/home/davedap/archivefs-*` worktrees plus
  `/home/davedap/emuwiz-bsfree-wii` and `/home/davedap/emuwiz-rename` remain
  merged/superseded cleanup candidates. Keep `/home/davedap/archivefs` (current
  main) and `/tmp/opencode/emuwiz-ar-licensing` (open #30).
- The current target directories remain about 80 GiB under the main worktree and
  28 GiB under the merged BSFree Wii worktree. The latter remains the clearest
  reproducible-output cleanup candidate.
- Local remote-tracking metadata now reports 50 exact merged branches excluding
  `origin/main`/symbolic HEAD, and 21 non-ancestor refs. Delete merged refs only
  in reviewed batches; compare squash/superseded non-ancestors and protect all
  open-PR heads, including #34 even if its head ref is not locally fetched.
- Mark dated reviews superseded rather than deleting historical evidence.

### Do not touch — refreshed compatibility surfaces

- `archivefs-core`, `archivefs-cli`, `archivefs-gui` package/crate identifiers.
- `archivefs-cli`, `archivefs-gui` and `emuwiz-gui` executable aliases.
- `~/.config/archivefs`, legacy data/cache/database/backup lookup and recovery
  paths, and mixed old/new-state reachability.
- `ARCHIVEFS_*` environment variables and compatibility fallbacks.
- Persisted/serialized/database/schema/provider/source/operation IDs, journal
  fields, transaction IDs and ownership markers.
- Historical release filenames/docs and the current canonical `archivefs-v*`
  artifact contract; #33 explicitly packages/verifies it.
- Approved `assets/branding/` bytes, the stable Linux app ID, desktop filename,
  icon ID and StartupWMClass landed by #33.
- Historical audits/research/release notes; annotate supersession instead of
  silently modernizing them.
- Conservative DAT Unknown/confidence/ambiguity, exact-match provenance,
  trusted roots, symlink/no-clobber checks, confirmation, cancellation, journals
  and rollback.
- Browse-only formats and blocked adapters until a separate reviewed write
  contract exists.

### Final refreshed recommendations

- **Recommended next cleanup PR:** PR 1, current-main product/repository docs and
  URL truth.
- **Recommended next reliability/feature PR:** PR 8, bounded ZIP-member DAT
  verification, only after #34 is technically reviewed and preserved.
- **Top 3 jobs for Claude Code:** (1) installer ownership-safe migration and
  uninstall; (2) review #34 and implement the later bounded ZIP contract; (3)
  encrypted AR provenance/licensing review. B2 design should also receive
  Claude review before implementation.
- **Top 3 jobs for DeepSeek:** (1) PR 1 docs/URL reconciliation; (2) prepare the
  narrow Wayland/beginner manual QA matrix for PR 12; (3) inventory #27 against
  already-merged rename evidence for a maintainer close/reduce decision.
- **Best next job for Codex:** PR 3, B1 classifier-version enforcement, followed
  by PR 11's compatibility/mutation regression suite after the schema settles.
- **Previously proposed PRs now obsolete because of #33:** the release-artifact
  naming ADR (old PR 12) and any standalone desktop/icon/app-ID/release-payload
  implementation. The old combined installer+desktop proposal is obsolete as a
  combined scope; only the ownership-safety half remains. The old GUI QA scope's
  app-ID/icon/X11 asset checks are done and should not be repeated.

### New top 5 actions

1. Correct current-main product/docs/repository URL drift.
2. Correct the public security boundary documentation.
3. Enforce B1 classifier versions across rename and organisation transactions.
4. Make landed binary/alias/desktop/icon install and uninstall ownership-safe.
5. Triage research/asset PRs: preserve #34, preserve-or-close #30 with YELLOW
   intact, reduce/close #27, and close #32 unless explicitly justified.

### Current blockers

- Encrypted AR: scheme-specific constant provenance/permission and independent
  known-answer vectors.
- PCSX2 ordinary downloadable cheats: licensed, immutable provider evidence.
- BSFree Wii real coverage: no Wii rows in the current admissible snapshot.
- Archive-aware implementation: #34 is preserved but still unreviewed/unmerged.
- B2 implementation: no accepted set identity/completeness/atomicity contract.
- Some live filesystem/Wayland QA requires suitable environments and legally
  redistributable fixtures.
