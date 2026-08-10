//! Platform-parameterized duplicate/conflict analysis for cheats that target
//! the shared Dolphin `GameSettings` structure.
//!
//! This is the generalization of the original GameCube-only
//! `analyze_bsfree_gamecube_duplicates`: the exact same two-pass algorithm
//! (source-level record/body/name passes, then output-level passes against the
//! live destination) is now expressed over a small normalized view
//! ([`DolphinCheat`]) that any Dolphin-targeted provider cheat can implement -
//! BSFree GameCube, BSFree Wii, GameHacking Wii, and so on. GameCube behaviour
//! is unchanged: `bsfree_gamecube::analyze_bsfree_gamecube_duplicates` is a
//! thin wrapper that feeds `BsFreeGameCubeCheat` values through this analyser.
//!
//! # Never deduplicated by display name
//!
//! Every finding is keyed off the *canonical code body digest* (the exact
//! lines a cheat contributes to the emulator file), never off its title. Two
//! providers routinely give the same cheat different names, and the same name
//! is routinely reused for unrelated cheats ("Level Select"), so a same-name
//! pair with a different body is reported as a conflict
//! (`DuplicateNameConflict` at source level, `SameLabelDifferentBody` against
//! the destination) and never silently collapsed.
//!
//! # Conflicting memory writes
//!
//! In addition to the original findings, the analyser detects two *different,
//! differently-labelled* cheats that both write to the same memory address and
//! size with different values ([`DolphinDedupFindingKind::ConflictingMemoryWrite`]).
//! Only provable direct writes (see `dolphin_code::derive_memory_operations`)
//! participate; pointer/conditional/master bodies never do, because their
//! target addresses cannot be proven. This finding blocks selection.

use std::collections::BTreeMap;

use serde::Serialize;

use super::dolphin_code::MemoryOperation;
use super::gecko_document::DolphinIniDocument;

/// Typed duplicate/conflict finding from the two-pass analysis described in
/// the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinDedupFindingKind {
    /// The exact same record (name + canonical body) appears more than once
    /// in the source game's own catalogue.
    DuplicateRecord,
    /// Two different source records share the same canonical code body before
    /// any conversion (different labels, identical code).
    DuplicateBody,
    /// Two different source records share the same name but differ in body.
    DuplicateNameConflict,
    /// After classification/conversion, two selected cheats resolve to
    /// byte-identical emulator output.
    ConvertedCollision,
    /// The cheat's final output body already exists in the destination under
    /// this same name (identical re-install; reported, not rewritten).
    AlreadyInstalled,
    /// The cheat's final output body already exists in the destination under
    /// a different name in the *same* section.
    AlreadyInstalledDifferentName,
    /// The cheat's final output body already exists in the destination in the
    /// *other* section (e.g. a Gecko-equivalent body that is already present
    /// as an Action Replay code). Uncertain equivalence; requires review.
    CrossSectionCollision,
    /// The cheat's name already exists in the destination with a different
    /// body. Never silently overwritten.
    SameLabelDifferentBody,
    /// Two different, differently-labelled cheats both write to the same
    /// memory address and size with different values. Blocked: enabling both
    /// would silently race, and EmuWiz never resolves it by priority.
    ConflictingMemoryWrite,
    /// The cheat is not a well-formed/installable format at all.
    NotInstallable,
}

impl DolphinDedupFindingKind {
    /// Whether a finding of this kind prevents the affected cheat from being
    /// staged. Everything here is a genuine safety refusal, never a cosmetic
    /// notice.
    #[must_use]
    pub const fn blocks_selection(self) -> bool {
        matches!(
            self,
            Self::DuplicateNameConflict
                | Self::CrossSectionCollision
                | Self::SameLabelDifferentBody
                | Self::ConflictingMemoryWrite
                | Self::NotInstallable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinDedupFinding {
    pub kind: DolphinDedupFindingKind,
    /// Provider-side identifier of the cheat this finding is about. Provenance
    /// is preserved so a reviewer can see which source produced it.
    pub cheat_upstream_id: i64,
    pub cheat_name: String,
    pub relates_to: Option<String>,
    pub detail: String,
}

/// A normalized, provider-agnostic view of one cheat that targets the shared
/// Dolphin `GameSettings` structure.
///
/// The dedup analyser needs exactly this; each provider adapter supplies its
/// own implementation so no provider-specific type leaks into the analysis.
pub trait DolphinCheat {
    fn upstream_id(&self) -> i64;
    /// The cheat's own display name (source label).
    fn name(&self) -> &str;
    /// The `"Name [Author]"` Dolphin display name this cheat would be written
    /// under in the destination file.
    fn dolphin_name(&self) -> String;
    /// Canonical uppercase hex-pair lines, in source order.
    fn code_lines(&self) -> &[String];
    /// Whether the cheat targets `[Gecko]` (`true`) or `[ActionReplay]`
    /// (`false`).
    fn target_gecko(&self) -> bool;
    /// SHA-256 over the exact lines this cheat contributes to the emulator
    /// file, combined with the section it targets.
    fn output_digest(&self) -> String;
    /// Whether the cheat is a well-formed, installable format.
    fn installable(&self) -> bool;
    /// The provable direct-write operations of this cheat's code body, used
    /// for `ConflictingMemoryWrite` detection.
    fn memory_operations(&self) -> Vec<MemoryOperation>;
}

/// Two-pass duplicate/conflict analysis for a set of classified, normalized
/// cheats against an existing Dolphin GameSettings document.
///
/// Accepts a heterogeneous slice of [`DolphinCheat`] views so records from
/// different providers (BSFree Wii and GameHacking Wii, for example) can be
/// analysed together; each provider implements the view over its own type.
///
/// Pass A (source-level) runs over the records alone: duplicate records,
/// duplicate bodies, and same-name-different-body records.
///
/// Pass B (output-level) runs over the *final output form* produced by any
/// classification: two records converting to identical Gecko/AR output, a
/// converted result colliding with an already-installed cheat in the same or
/// the other section, same-name-different-body collisions with installed
/// content, and - as a dedicated finding - two different cheats writing
/// different values to the same provable memory address.
pub fn analyze_dolphin_duplicates(
    cheats: &[&dyn DolphinCheat],
    destination: &DolphinIniDocument,
) -> Vec<DolphinDedupFinding> {
    let mut findings = Vec::new();

    // Pass A: source-level duplicates.
    let mut by_record: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    let mut by_body: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, cheat) in cheats.iter().enumerate() {
        by_record
            .entry((cheat.name().to_string(), cheat.output_digest()))
            .or_default()
            .push(index);
        by_body
            .entry(cheat.output_digest())
            .or_default()
            .push(index);
        by_name
            .entry(cheat.name().to_string())
            .or_default()
            .push(index);
    }
    for indices in by_record.values() {
        for (offset, index) in indices.iter().skip(1).enumerate() {
            let cheat = &cheats[*index];
            findings.push(DolphinDedupFinding {
                kind: DolphinDedupFindingKind::DuplicateRecord,
                cheat_upstream_id: cheat.upstream_id(),
                cheat_name: cheat.name().to_string(),
                relates_to: indices.first().map(|i| cheats[*i].upstream_id().to_string()),
                detail: format!(
                    "the exact same record appears again (occurrence {}); only the first is ever installed",
                    offset + 2
                ),
            });
        }
    }
    for (digest, indices) in &by_body {
        if indices.len() > 1 {
            for index in indices.iter().skip(1) {
                let cheat = &cheats[*index];
                findings.push(DolphinDedupFinding {
                    kind: DolphinDedupFindingKind::DuplicateBody,
                    cheat_upstream_id: cheat.upstream_id(),
                    cheat_name: cheat.name().to_string(),
                    relates_to: indices
                        .first()
                        .map(|i| format!("{} ({})", cheats[*i].name(), cheats[*i].upstream_id())),
                    detail: "a different label carries the same code body; the labels are \
                             variants, not independent cheats"
                        .to_string(),
                });
            }
        }
        let _ = digest;
    }
    for (name, indices) in &by_name {
        let mut bodies = indices
            .iter()
            .map(|i| cheats[*i].output_digest())
            .collect::<Vec<_>>();
        bodies.sort();
        bodies.dedup();
        if bodies.len() > 1 {
            for index in indices {
                let cheat = &cheats[*index];
                findings.push(DolphinDedupFinding {
                    kind: DolphinDedupFindingKind::DuplicateNameConflict,
                    cheat_upstream_id: cheat.upstream_id(),
                    cheat_name: cheat.name().to_string(),
                    relates_to: None,
                    detail: format!(
                        "the game contains multiple cheats named {:?} with different bodies; \
                         these are conflicts, not independent cheats",
                        name
                    ),
                });
            }
        }
    }

    // Pass B: output-level against the destination.
    let installed_gecko = installed_bodies(destination, true);
    let installed_ar = installed_bodies(destination, false);
    let installed_names_gecko = installed_names(destination, true);
    let installed_names_ar = installed_names(destination, false);

    for cheat in cheats {
        let target_gecko = cheat.target_gecko();
        let (same_section_bodies, other_section_bodies) = if target_gecko {
            (&installed_gecko, &installed_ar)
        } else {
            (&installed_ar, &installed_gecko)
        };
        let dolphin_name = cheat.dolphin_name();

        if let Some((existing_name, _)) = same_section_bodies
            .iter()
            .find(|(_, lines)| *lines == cheat.code_lines())
        {
            let kind = if *existing_name == dolphin_name {
                DolphinDedupFindingKind::AlreadyInstalled
            } else {
                DolphinDedupFindingKind::AlreadyInstalledDifferentName
            };
            findings.push(DolphinDedupFinding {
                kind,
                cheat_upstream_id: cheat.upstream_id(),
                cheat_name: cheat.name().to_string(),
                relates_to: Some(existing_name.clone()),
                detail: if *existing_name == dolphin_name {
                    "the same code is already installed under this name; re-installing is a \
                     no-op"
                        .to_string()
                } else {
                    format!(
                        "an identical code is already installed under {:?}; it will not be \
                         installed a second time",
                        existing_name
                    )
                },
            });
            continue;
        }

        if let Some((existing_name, _)) = other_section_bodies
            .iter()
            .find(|(_, lines)| *lines == cheat.code_lines())
        {
            findings.push(DolphinDedupFinding {
                kind: DolphinDedupFindingKind::CrossSectionCollision,
                cheat_upstream_id: cheat.upstream_id(),
                cheat_name: cheat.name().to_string(),
                relates_to: Some(existing_name.clone()),
                detail: format!(
                    "the same hex-pair body is already installed in the other Dolphin section \
                     under {:?}; the two engines interpret these bytes differently, so this is \
                     reported for review and not applied",
                    existing_name
                ),
            });
            continue;
        }

        let same_section_has_name = if target_gecko {
            installed_names_gecko.contains(&dolphin_name)
        } else {
            installed_names_ar.contains(&dolphin_name)
        };
        // A name collision is a conflict regardless of which section it sits
        // in: two same-named codes with different bodies in the two Dolphin
        // engines would both be enableable, and silently enabling one of them
        // must never happen. Body-equal cases already returned above, so any
        // same name here is necessarily a different body.
        let other_section_has_name = if target_gecko {
            installed_names_ar.contains(&dolphin_name)
        } else {
            installed_names_gecko.contains(&dolphin_name)
        };
        if same_section_has_name || other_section_has_name {
            findings.push(DolphinDedupFinding {
                kind: DolphinDedupFindingKind::SameLabelDifferentBody,
                cheat_upstream_id: cheat.upstream_id(),
                cheat_name: cheat.name().to_string(),
                relates_to: Some(dolphin_name.clone()),
                detail: format!(
                    "a code named {:?} is already installed (in {} the {}) with a different \
                     body; EmuWiz will not overwrite it",
                    dolphin_name,
                    if same_section_has_name {
                        "same"
                    } else {
                        "other"
                    },
                    if target_gecko {
                        "Gecko section"
                    } else {
                        "ActionReplay section"
                    }
                ),
            });
        }
    }

    // Conflicting memory writes: two *different* cheats that both write to the
    // same address+size with a different value. Only provable direct writes
    // count; the same cheat writing to itself is excluded by pairing only
    // distinct cheats. This never deduplicates by title - it keys on the
    // provable write target, exactly the opposite of display-name identity.
    for (index, cheat) in cheats.iter().enumerate() {
        let operations = cheat.memory_operations();
        if operations.is_empty() {
            continue;
        }
        for other in cheats.iter().skip(index + 1) {
            if cheat.upstream_id() == other.upstream_id() {
                continue;
            }
            for own in &operations {
                let other_operations = other.memory_operations();
                let conflicting = other_operations.iter().find(|candidate| {
                    candidate.address == own.address && candidate.size == own.size
                });
                if let Some(candidate) = conflicting
                    && candidate.value != own.value
                {
                    findings.push(DolphinDedupFinding {
                        kind: DolphinDedupFindingKind::ConflictingMemoryWrite,
                        cheat_upstream_id: cheat.upstream_id(),
                        cheat_name: cheat.name().to_string(),
                        relates_to: Some(format!("{} ({})", other.name(), other.upstream_id())),
                        detail: format!(
                            "{:?} and {:?} both write to 0x{:08X} ({} bytes) with different \
                             values (0x{:08X} vs 0x{:08X}); enabling both would race, so this \
                             is blocked and must be resolved explicitly",
                            cheat.name(),
                            other.name(),
                            own.address,
                            own.size,
                            own.value,
                            candidate.value
                        ),
                    });
                    break;
                }
            }
        }
    }

    // A cheat that cannot be installed is reported once regardless of any
    // other finding, so a browse-only record is never silently dropped.
    for cheat in cheats {
        if !cheat.installable() {
            findings.push(DolphinDedupFinding {
                kind: DolphinDedupFindingKind::NotInstallable,
                cheat_upstream_id: cheat.upstream_id(),
                cheat_name: cheat.name().to_string(),
                relates_to: None,
                detail: "this record is not a well-formed installable format; browse-only"
                    .to_string(),
            });
        }
    }

    findings
}

/// Maps existing destination `[Gecko]`/`[ActionReplay]` code names to their
/// canonical line bodies, so a name lookup is a `BTreeMap`.
fn installed_bodies(document: &DolphinIniDocument, gecko: bool) -> Vec<(String, Vec<String>)> {
    let codes = if gecko {
        &document.gecko_codes
    } else {
        &document.action_replay_codes
    };
    codes
        .iter()
        .map(|code| (code.name.clone(), code.lines.clone()))
        .collect()
}

fn installed_names(document: &DolphinIniDocument, gecko: bool) -> Vec<String> {
    let codes = if gecko {
        &document.gecko_codes
    } else {
        &document.action_replay_codes
    };
    codes.iter().map(|code| code.name.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    struct FakeCheat {
        id: i64,
        name: String,
        lines: Vec<String>,
        gecko: bool,
        installable: bool,
    }

    impl DolphinCheat for FakeCheat {
        fn upstream_id(&self) -> i64 {
            self.id
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn dolphin_name(&self) -> String {
            format!("{} [Test]", self.name)
        }
        fn code_lines(&self) -> &[String] {
            &self.lines
        }
        fn target_gecko(&self) -> bool {
            self.gecko
        }
        fn output_digest(&self) -> String {
            let mut hasher = sha2::Sha256::new();
            hasher.update(if self.gecko {
                &b"gecko\n"[..]
            } else {
                &b"actionreplay\n"[..]
            });
            for line in &self.lines {
                hasher.update(line.as_bytes());
                hasher.update(b"\n");
            }
            super::super::dolphin_code::hex_sha256(&hasher.finalize())
        }
        fn installable(&self) -> bool {
            self.installable
        }
        fn memory_operations(&self) -> Vec<MemoryOperation> {
            super::super::dolphin_code::derive_memory_operations(&self.lines)
        }
    }

    fn cheat(id: i64, name: &str, line: &str, gecko: bool) -> FakeCheat {
        FakeCheat {
            id,
            name: name.to_string(),
            lines: vec![line.to_string()],
            gecko,
            installable: true,
        }
    }

    fn empty_destination() -> DolphinIniDocument {
        super::super::gecko_document::parse_dolphin_ini("")
    }

    #[test]
    fn conflicting_memory_writes_are_detected_and_block_selection() {
        let destination = empty_destination();
        let cheats = [
            cheat(1, "Max Money", "042318AC 3B8003E7", true),
            cheat(2, "No Money", "042318AC 00000000", true),
        ];
        let views: Vec<&dyn DolphinCheat> = cheats
            .iter()
            .map(|cheat| cheat as &dyn DolphinCheat)
            .collect();
        let findings = analyze_dolphin_duplicates(&views, &destination);
        assert!(
            findings
                .iter()
                .any(|finding| { finding.kind == DolphinDedupFindingKind::ConflictingMemoryWrite }),
            "two cheats writing different values to the same address must conflict: \
             {findings:?}"
        );
        assert!(DolphinDedupFindingKind::ConflictingMemoryWrite.blocks_selection());
    }

    #[test]
    fn same_address_same_value_is_a_duplicate_body_not_a_conflict() {
        let destination = empty_destination();
        let cheats = [
            cheat(1, "Infinite Health", "042318AC 3B8003E7", true),
            cheat(2, "999 HP", "042318AC 3B8003E7", true),
        ];
        let views: Vec<&dyn DolphinCheat> = cheats
            .iter()
            .map(|cheat| cheat as &dyn DolphinCheat)
            .collect();
        let findings = analyze_dolphin_duplicates(&views, &destination);
        assert!(
            !findings
                .iter()
                .any(|finding| finding.kind == DolphinDedupFindingKind::ConflictingMemoryWrite),
            "identical writes are not a conflict: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == DolphinDedupFindingKind::DuplicateBody),
            "identical body under a different name is a duplicate body: {findings:?}"
        );
    }

    #[test]
    fn same_name_different_body_is_never_collapsed() {
        let destination = empty_destination();
        let cheats = [
            cheat(1, "Level Select", "042318AC 00000001", true),
            cheat(2, "Level Select", "0424CD50 00000002", true),
        ];
        let views: Vec<&dyn DolphinCheat> = cheats
            .iter()
            .map(|cheat| cheat as &dyn DolphinCheat)
            .collect();
        let findings = analyze_dolphin_duplicates(&views, &destination);
        assert!(
            findings
                .iter()
                .any(|finding| { finding.kind == DolphinDedupFindingKind::DuplicateNameConflict }),
            "same display name with different bodies must be a conflict: {findings:?}"
        );
    }

    #[test]
    fn cross_section_duplicate_is_reported_for_review() {
        let destination = super::super::gecko_document::parse_dolphin_ini(
            "[ActionReplay]\n$Installed\n042318AC 3B8003E7\n",
        );
        let cheats = [cheat(1, "Health", "042318AC 3B8003E7", true)];
        let views: Vec<&dyn DolphinCheat> = cheats
            .iter()
            .map(|cheat| cheat as &dyn DolphinCheat)
            .collect();
        let findings = analyze_dolphin_duplicates(&views, &destination);
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == DolphinDedupFindingKind::CrossSectionCollision),
            "{findings:?}"
        );
    }
}
