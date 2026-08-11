# EmuWiz loose-ends / project-health audit

Audit date: 2026-08-11. Original code baseline: `origin/main` at
`f7c450cad3b89207251dd3b2b4747af1f1e01d42` (merge of PR #29), refreshed
against `7c8d6ea1891d4bd32bcdb0716ff7d998ec08ed83` (merge of PR #33), and
refreshed again against `5c41d99f3af85569ef0393edfd33ecde5b4eb582` (merge
of PR #36, landed immediately after PR #34). This remains a read-only
audit; the only workspace changes across every pass are edits to this
report.

**Only the "Cleanup execution plan — current" section at the end of this
report is current guidance.** The sections above it are the original
2026-08-11 findings, kept as an evidentiary record of what was flagged and
why, but they were superseded rather than re-verified by the later passes.
Several of their specific claims no longer hold — PR #33 described as
unmerged, no archive-aware research preserved anywhere, and a ranked
P0/P1/P2 board, top-5 actions and blockers list that predate #33/#34/#36 —
so read everything before the final section as history, not as a task
list. A full pre-#33 planning snapshot and its own PR-state table
previously sat between the executive summary and the final section; it
added nothing not already carried, more accurately, by the current section
below, so it has been removed here rather than kept behind a banner — see
this file's git history if the original wording is ever needed.

## Executive summary

EmuWiz is healthier than its roadmap and current release prose suggest. The
core safety architecture is substantial, tests are numerous, and the recent DAT,
Cheats & Mods, branding, and beginner-workflow work is present on `main`.
The main risk before another large feature cycle is not an unfinished feature;
it is loss of a trustworthy description of what is already shipped.

The highest-value close-out is: correct the security/product docs, add the
one missing Games-only stale-classifier gate, and make install/uninstall
ownership-safe. Research-PR triage that was open at the time of the
original audit (#27, #30, #32, #34) has since moved: #34 and its CHD
companion #36 are merged, #27 is closed as superseded, and only #30/#32
remain open — see the current section for their disposition. The next
technical feature should be a small archive-aware DAT verification slice
built on #34/#36's preserved research, not another broad adapter.

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
| Desktop/app icon integration | Landed via merged #33: Linux application ID, embedded 256px icon, desktop launcher, hicolor icon install, and release packaging/verification | **Done** |
| Archive-aware DAT verification, NES header normalization, ZIP-member verification, CHD verification | Research preserved and merged via #34 (general archive-aware architecture: ZIP member selection, NES header normalization, provenance model) and #36 (CHD-specific companion adapter, explicitly not a parallel engine); no implementation code exists yet, current audit still hashes each outer file as-is | **Researched only (preserved); implementation not started** |
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
4. archive-aware and CHD DAT verification research is preserved and merged
   (#34/#36), but implementation itself has not started (P1 capability and
   false-negative risk, not a current vulnerability);
5. live/integration evidence is weaker than the unit-test count suggests.

RomM token handling is comparatively strong: token files require private mode,
symlinks/public files are refused, values are redacted, redirects are refused,
and tests exercise header-only use. No token-handling follow-up is recommended
without new evidence.

## Cleanup execution plan — current (refreshed after #33, #34, #36)

Baseline: `origin/main` at `5c41d99f3af85569ef0393edfd33ecde5b4eb582` (merge
of PR #36, landed immediately after PR #34's merge). This supersedes both
the original 2026-08-11 findings above and the first refresh (against
`7c8d6ea1891d4bd32bcdb0716ff7d998ec08ed83`, merge of PR #33) — this is the
only section that reflects current `main`, and the only one that should be
treated as a task list.

Live open PRs relevant to this audit are #30 (encrypted Action Replay
licensing research, still an open draft) and #32 (ImgBot, open,
non-draft). #27 (compatibility-rename research) has been **closed** as
superseded. #34 (archive-aware DAT verification research) and #36 (its CHD
companion) are **merged**. No other open research PR relevant to this
audit's scope was found.

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

### What PR #34 and #36 resolved

The following are **DONE** on current `main` as of
`5c41d99f3af85569ef0393edfd33ecde5b4eb582`:

- `docs/research/ARCHIVE_AWARE_DAT_VERIFICATION_RESEARCH.md` (#34, merged at
  `91109cb5e46c27bc97f7d6a9439025647a74e129`) is a reviewed, citation-backed
  design for ZIP-aware DAT verification: reuse of existing archive
  opening/enumeration and bounded-read primitives, member selection and
  ambiguity rules, NES header normalization, a two-axis result/provenance
  model (`AuditVerdict` unchanged + new `MatchProvenance`), a cache-key
  design, and a phased P0/P1 implementation plan. No production code — this
  is design only.
- `docs/research/CHD_VERIFICATION_IMPLEMENTATION_RESEARCH.md` (#36, merged
  at `407b42e6db73f4b0e9d51c36ec8e747dc50266d0`) is an explicit companion to
  #34: CHD format research, a native-Rust-reader recommendation, and
  CHD-specific result states that plug into #34's same two-axis model
  rather than a parallel verification engine. Its own phased P0/P1/P2 plan
  follows the same discipline. No production code.
- The original audit's "no preserved research artifact" and "research not
  yet reviewed" findings are therefore **resolved**: archive-aware and CHD
  verification research is now reviewed, merged, and durably preserved.
  What remains outstanding is implementation itself — bounded ZIP-member
  hashing, NES normalization, and the CHD adapter are all still **not
  started** on `main`. See PR 8/PR 9 in the active queue below.

### Refreshed ranked cleanup board

#### P0 — before the next major feature

| Item | Current status on `5c41d99` | Next action | Risk / type / size |
|---|---|---|---|
| Current product/docs truth: `README.md:73,112-128,162-188,205-211`, `CHANGELOG.md:98-99`, `ROADMAP.md:197,224-225,268-274`, `docs/ADAPTER_SUPPORT_MATRIX.md`, `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md`, `docs/reviews/EMUWIZ_RENAME_AUDIT.md:31`, `docs/RELEASE_ENGINEERING.md:214` | Still stale. Old repo URLs, PCSX2/BSFree behavior claims, roadmap state and `archivefs-cli doctor` living guidance remain unless a separate current-docs PR addresses them. | One current-main documentation reconciliation PR. Preserve historical release notes and compatibility examples. | **LOW**; docs/cleanup; small |
| `SECURITY.md:31-58`, `docs/security.md` | Still says PCSX2 metadata is the only network use and mutation is future/read-only. | Rewrite the public boundary around actual opt-in network clients, confirmed mutation, tokens, caches, journals and rollback. | **MEDIUM**; docs/security; small |
| Games-only B1 classifier version in `dat/classification.rs`, rename plan/apply and organisation transaction | Unchanged: plans record `CLASSIFIER_VERSION`; `RenameTransaction` and apply gates still enforce generation only. | Carry/enforce the reviewed version with conservative old-journal handling and no-mutation mismatch tests. | **HIGH**; implementation/compatibility/safety/tests; small |
| Installer ownership in `install.sh` and `tests/test_install.sh` | Still unresolved and broader after #33. `remove_owned_path` is named as ownership-aware but checks only object kind/existence; foreign same-name files/symlinks can be overwritten or deleted. | Add manifest/content/symlink ownership proof, refuse or back up foreign targets, and support pre-manifest upgrades/uninstall. | **HIGH**; implementation/compatibility/safety/tests; medium |
| Open-PR/research close-out (#30/#32) | #27 closed, #34/#36 merged — no action needed on any of those. #30 and #32 remain open. | Preserve-or-close #30 with YELLOW intact; close #32 unless separately justified. | **LOW**; review/cleanup |

#### P1 — soon

| Item | Current status on `5c41d99` | Next action | Risk / type / size |
|---|---|---|---|
| ZIP-member DAT verification | Audit still hashes outer files; #34's research is merged but unimplemented. | Implement bounded ZIP Stored/Deflate member evidence per #34's P0 plan. | **HIGH**; implementation/safety/tests; medium |
| NES normalization | No raw-first iNES/headerless normalization exists. | Separate PR after the evidence model is stable; never rewrite source bytes. | **HIGH**; implementation/compatibility/tests; medium |
| CHD verification | No CHD reader/adapter exists; #36's research is merged but unimplemented. | Implement per #36's P0 plan (header identity + streaming integrity, no extraction), as a container adapter inside #34's pipeline, not a parallel engine. | **HIGH**; implementation/safety/tests; medium |
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
- PR #34's archive-aware DAT verification research and PR #36's CHD
  companion research are landed (see "What PR #34 and #36 resolved" above);
  implementation itself remains open work (P1 table above, PR 8/PR 9 below).
- PR #27 is closed as superseded.
- Approved EmuWiz branding and Games-only P0 remain landed.
- `archivefs-v*` release artifact naming is an intentional enforced
  compatibility surface, not a pending rename.
- The refreshed `TODO`/`FIXME`/`HACK` search found only historical audit prose;
  there is no new live code marker to queue. GUI "follow-up" comments (Select
  all visible, text-field context menus, clipboard failure handling, Unmount
  selected confirmation) describe work that is already implemented and are
  stale/historical comments, not open defects. "Later" in installer prompts,
  rollback guidance, retry copy and cache docs is ordinary user wording, not a
  deferred engineering item.
- No genuinely stale live GUI/CLI `ArchiveFS` product label was found. Current
  repository URLs and one living release command are docs drift; other hits are
  compatibility/history/ownership/package identifiers.
- RomM token handling and the completed DAT rename/organisation engines have no
  new #33/#34/#36-related loose end.

### Revised active cleanup queue

There are **11 active reviewable PR-sized jobs, plus 1 done** (PR 5, below —
superseded by #34/#36's merge), down from 13 originally. The old release
artifact naming ADR (previous PR 12) is obsolete because #33 deliberately
codified and tests `archivefs-v*`. The old combined "installer ownership +
desktop integration" proposal is obsolete in that form: desktop integration is
done, while the narrower ownership-safety job remains. PR numbering is kept
stable rather than renumbered after removing completed items, so the
dependency references in PR 8/PR 9 below stay correct.

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

#### PR 5 — Review and preserve archive-aware DAT research (#34) — DONE

Merged as #34 (general archive-aware architecture) and its CHD-specific
companion #36 (see "What PR #34 and #36 resolved" above). No further
action. Retained here, marked done, so the dependency numbering in PR 8/PR 9
below stays stable and traceable rather than being renumbered.

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
- **Dependencies/risk:** #34's research is merged, so this can proceed
  directly; **HIGH**, implementation/safety/tests, medium.
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
- **Dependencies/risk:** #34's research is merged; PR 8 for archive members;
  **HIGH**, implementation/compatibility/tests, medium.
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
- **Out of scope:** production models, GUI/apply, loose "Disc" matching,
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
- #27 is already closed as superseded — no further action needed.
- Preserve or close #30 (still open, unmerged); #34 and #36 are merged and
  need no further preservation action.
- The former PR #33 branch `feature/emuwiz-linux-desktop-icon` is merged and
  no longer owns the primary worktree; it, along with the merged #34/#36
  research branches, are normal branch-deletion candidates after confirming
  no follow-up commit exists.
- Mark dated reviews superseded rather than deleting historical evidence.
- Prior passes of this audit also recorded specific worktree/build-output
  and remote-branch cleanup candidates (missing worktree registrations,
  ~80 GiB/~28 GiB target directories, ~50 exact-ancestor remote branches).
  That host-state inventory was not re-verified for this pass; re-run a
  fresh `git branch -r --merged origin/main` / worktree listing before
  acting on it rather than trusting the earlier numbers, and protect every
  currently open PR head (#30, #32) regardless of what any earlier count
  said.

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
  verification — #34's research is merged, so this can proceed directly with
  no remaining research-preservation blocker.
- **Top 3 jobs for Claude Code:** (1) installer ownership-safe migration and
  uninstall; (2) implement the bounded ZIP contract (PR 8) and NES
  normalization (PR 9) now that #34/#36 are merged; (3) encrypted AR
  provenance/licensing review (#30). B2 design should also receive Claude
  review before implementation.
- **Top 3 jobs for DeepSeek:** (1) PR 1 docs/URL reconciliation; (2) prepare the
  narrow Wayland/beginner manual QA matrix for PR 12; (3) none outstanding on
  #27 — it is already closed.
- **Best next job for Codex:** PR 3, B1 classifier-version enforcement, followed
  by PR 11's compatibility/mutation regression suite after the schema settles.
- **Previously proposed PRs now obsolete:** the release-artifact naming ADR
  (old PR 12) and any standalone desktop/icon/app-ID/release-payload
  implementation are obsolete because of #33. The research-review PR (PR 5)
  is obsolete because of #34/#36's merge. The old combined
  installer+desktop proposal is obsolete as a combined scope; only the
  ownership-safety half remains. The old GUI QA scope's app-ID/icon/X11
  asset checks are done and should not be repeated.

### New top 5 actions

1. Correct current-main product/docs/repository URL drift.
2. Correct the public security boundary documentation.
3. Enforce B1 classifier versions across rename and organisation transactions.
4. Make landed binary/alias/desktop/icon install and uninstall ownership-safe.
5. Triage remaining research/asset PRs: #34/#36 already merged and #27
   already closed need no action; preserve-or-close #30 with YELLOW intact;
   close #32 unless explicitly justified.

### Current blockers

- Encrypted AR: scheme-specific constant provenance/permission and independent
  known-answer vectors.
- PCSX2 ordinary downloadable cheats: licensed, immutable provider evidence.
- BSFree Wii real coverage: no Wii rows in the current admissible snapshot.
- B2 implementation: no accepted set identity/completeness/atomicity contract.
- Archive-aware/CHD implementation is **not** blocked on research anymore —
  #34 and #36 are merged. The remaining blocker is simply that
  implementation (bounded ZIP-member hashing, NES normalization, the CHD
  adapter) has not been written yet; see PR 8/PR 9 above.
- Some live filesystem/Wayland QA requires suitable environments and legally
  redistributable fixtures.
