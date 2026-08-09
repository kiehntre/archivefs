# Beta 1 beginner / gamer view review

This pass follows the Sunshine/XFCE hands-on session after PR #20. It changes presentation and one confirmed Doctor classification boundary; DAT matching, providers, canonical organisation, and apply/rollback semantics are unchanged.

| Observed issue | Before | Now |
| --- | --- | --- |
| Home icons | Emoji depended on the desktop fallback font and rendered as square boxes. | Primary and secondary navigation use short printable-ASCII compositions beside permanent text labels. |
| Home hierarchy | The six workflows looked like equally grey panels. | Each primary workflow has a fixed, restrained concept accent and faint top/tint treatment; status badges retain their independent semantic colours and secondary cards remain quiet. |
| DAT source messages | “verification notes” did not say what happened or whether the catalogue worked. | “catalogue files need attention” / “catalogue issues found” is followed by “The catalogue still works”; parser messages and locations are under **What happened?** and **Technical details**. |
| Loose ROM Doctor group | The healthy headline was followed immediately by reason, media, evidence, and paths. | The default is the exact healthy count plus “These games can be used directly. Nothing needs fixing.” Examples and the complete technical breakdown have separate disclosures. |
| Missing `.cue` rows | A legacy `.cue` archive row survived in SQLite. Because current scans correctly do not discover cue sheets as games, the next successful scan marked that old row missing and Doctor converted it into a missing-game finding. | Doctor recognises persisted `.cue`/`.m3u` rows as known disc companion metadata and does not turn them into missing-game findings. The row and scan history remain intact. No BIN/CUE pairing is inferred. A regression test reproduces the legacy-row → current successful scan → missing-history flow. |
| Settings | “Intentionally unavailable” explained implementation status. | A short “More settings coming later” note replaces the engineering explanation. |
| History | Cards led with operation IDs, Unix timestamps, roots, source paths/mode, and journal paths. | Cards lead with the game, emulator, local human time, change count, and rollback action. Raw IDs, timestamp, paths, mode, and per-entry audit data remain under **Technical details**. |
| Rename safety | Some DAT text claimed ArchiveFS never renames files. | DAT policy consistently says: “Your files won't be renamed unless you approve it.” |
| Cheats & Mods | The normal route exposed archive context, profile discovery, trusted retrieval, gating, and implementation stages. | The empty state starts with “Choose a game”. Selected routes identify the game, ask for the relevant profile/source in plain language, and move workflow/audit state under diagnostics. Empty matching says “No cheats found for this game.” |
| BSFree | Correct browse-only capability was surrounded by implementation-stage language. | It says “Browse only” and clearly states that ArchiveFS can search the historical database but cannot install from BSFree yet. |
| Scope | Global/platform effects had to be inferred from selectors. | DAT policy shows both “Applies to: All platforms” and “Editing: Global defaults” (or the selected platform). Cheat pages label the selected game explicitly. |

## Glyph fallback strategy

No font is bundled. Workflow marks are restricted to printable ASCII, so they render through egui's normal text font without an emoji fallback. Labels remain the authoritative navigation text. The Home test checks every primary composition against that contract.

## Deliberately deferred

- No new artwork or theme system was introduced; a future illustration pass can build on the fixed workflow accents.
- BSFree installation remains unsupported.
- Unfinished provider and settings capabilities remain unavailable; their technical state is retained only where a details view needs it.
- The fix does not infer disc membership from neighbouring filenames and does not alter stored scan evidence.
