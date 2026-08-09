//! BSFree GameCube → Dolphin adapter bridge.
//!
//! This is the only place where raw BSFree Archive records become
//! installable Dolphin GameSettings content. It is deliberately a thin,
//! conservative seam on top of the *existing* GameCube GameHacking install
//! adapter ([`stage_gamecube_gamehacking_install`]) and the shared
//! preview/apply/rollback transaction pipeline. It does not invent a second
//! installation engine, and it never writes an emulator file directly.
//!
//! # What is safe to install (the proven subset)
//!
//! A BSFree code is only ever offered for installation when it is a
//! well-formed `XXXXXXXX YYYY` hex-pair Action Replay code for GameCube
//! (BSFree device "Action Replay"), with no master codes, no zero codes, no
//! self-modifying codes, and no placeholder text. Within that subset:
//!
//! - **`GeckoEquivalent`** – every line is an Action Replay *32-bit RAM
//!   write* (`04XXXXXX YYYYYYYY`) whose address fits Gecko's 24-bit address
//!   field. Dolphin's own sources prove these bytes are executed identically
//!   under `[Gecko]` and `[ActionReplay]`:
//!   - `ActionReplay.cpp` decodes the first word's bit fields
//!     (`subtype:2 | type:3 | size:2 | gcaddr:25`); type 0 / subtype 0 /
//!     size 2 is a single 32-bit write to `gcaddr | 0x80000000`.
//!   - The Gecko code handler (`docs/codehandler.s`) decodes `04XXXXXX` as
//!     code type 0 / sub type 2 – a single `stw` of `YYYYYYYY` to
//!     `0x80000000 + XXXXXX`.
//!
//!     When `gcaddr < 0x01000000` the two write the same value to the same
//!     address, so the exact same lines may be emitted into `[Gecko]` with no
//!     transformation. The "conversion" is therefore byte-identity.
//! - **`ActionReplayNative`** – every other well-formed hex-pair code that
//!   Dolphin's Action Replay engine implements (16/8-bit and float RAM
//!   writes, pointer writes, add codes, conditionals). These are emitted
//!   verbatim into `[ActionReplay]` and never relabelled as Gecko.
//!
//! # What is never installed
//!
//! - `Unsupported` – well-formed hex pairs that contain an Action Replay
//!   command Dolphin refuses at runtime (master codes, zero codes,
//!   self-modifying codes).
//! - `Malformed` – anything that is not a well-formed hex-pair line
//!   (placeholders such as `XXXX`/`?`/`N/A`, the base-31 encrypted
//!   `XXXX-XXXX-XXXXX` AR codes, free text). The encrypted dash-format codes
//!   are real Action Replay content that Dolphin could decrypt, but ArchiveFS
//!   has no verified decryptor and therefore cannot inspect what they decode
//!   to; they remain browse-only.
//!
//! Every other BSFree system/device pairing (PS2 CodeBreaker/GameShark/ARMax
//! and every retro platform) stays browse-only: no existing adapter can
//! represent those formats, and this module does not invent a translator.
//!
//! # Identity is the selected archive's, never BSFree's
//!
//! BSFree records carry no emulator-stable identifier (no Dolphin Game ID,
//! no serial, no CRC, no hash). The destination is therefore keyed by the
//! *selected game archive's* verified Dolphin Game ID, exactly like the
//! existing GameCube GameHacking adapter. The BSFree game contributes only
//! platform + normalized title + version/region evidence, which must be
//! confirmed by the user before any Apply; nothing here ever applies
//! automatically. See [`bsfree_gamecube_match`] and the CLI flow.

use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::ReadOnlyCheatCatalogue;
use super::bsfree::{BSFREE_UPSTREAM_PROJECT, BsFreeCatalogue, BsFreeCheat, BsFreeError};
use super::gamehacking_gamecube_install_plan::{
    GameCubeCheatSelection, GameCubeGameHackingInstallPreview,
    GameCubeGameHackingInstallPreviewRequest, GameCubeInstallPlanError,
    GameCubeInstallPlanErrorKind, StagedGameCubeIni, build_dolphin_gamehacking_install_preview,
    stage_gamecube_gamehacking_install,
};
use super::gamehacking_gamecube_provider::{GameCubeCodeFormat, GameHackingGameCubeCheat};
use super::gecko_document::{DolphinIniDocument, is_gecko_code_line};

pub const BSFREE_GAMECUBE_PROVIDER_LABEL: &str = "BSFree Archive";

/// One BSFree GameCube cheat's classification, decided strictly from the
/// authoritative Action Replay and Gecko semantics above - never guessed
/// from title, never inferred from a loose resemblance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeGameCubeCodeFormat {
    /// Every line is an Action Replay 32-bit RAM write (`04XXXXXX YYYY`)
    /// whose address fits Gecko's 24-bit address field. Dolphin's own
    /// sources show the exact same bytes execute identically under both
    /// engines, so this code may be emitted into `[Gecko]` unchanged.
    GeckoEquivalent,
    /// A well-formed hex-pair Action Replay code that Dolphin's AR engine
    /// implements, emitted verbatim into `[ActionReplay]`. Never converted.
    ActionReplayNative,
    /// Well-formed hex pairs containing an Action Replay command Dolphin
    /// refuses at runtime (master/zero/self-modifying codes).
    Unsupported,
    /// Not a well-formed `XXXXXXXX YYYY` hex-pair code (placeholders,
    /// encrypted dash-format codes, free text).
    Malformed,
}

impl BsFreeGameCubeCodeFormat {
    #[must_use]
    pub const fn is_installable(self) -> bool {
        matches!(self, Self::GeckoEquivalent | Self::ActionReplayNative)
    }

    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::GeckoEquivalent => {
                "Every line is an Action Replay 32-bit RAM write with a 24-bit address; \
                 Dolphin executes the identical bytes the same way under Gecko, so this \
                 code is installed as a native Gecko code with no transformation."
            }
            Self::ActionReplayNative => {
                "A well-formed GameCube Action Replay hex-pair code. Installed verbatim \
                 into Dolphin's [ActionReplay] section; it is not relabelled as Gecko."
            }
            Self::Unsupported => {
                "The code body contains an Action Replay command Dolphin refuses to run \
                 (a master code, zero code, or self-modifying code). Browse-only."
            }
            Self::Malformed => {
                "The code body is not a well-formed X XXXXXXXX YYYYYYYY hex-pair code \
                 (placeholders, encrypted codes, or free text). Browse-only."
            }
        }
    }
}

/// Per-line Action Replay command family, decoded from the first word's bit
/// fields exactly as Dolphin's `ActionReplay.cpp` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArLineFamily {
    Write8,
    Write16,
    Write32,
    WriteFloat,
    WritePointer,
    AddCode,
    MasterCode,
    Conditional,
    ZeroCode,
    SelfModifying,
    Malformed,
}

/// Decodes one `XXXXXXXX YYYY` hex-pair line under the GameCube Action
/// Replay bit layout (`subtype:2 | type:3 | size:2 | gcaddr:25`).
fn ar_line_family(line: &str) -> ArLineFamily {
    let mut pieces = line.split_whitespace();
    let (Some(first), Some(second)) = (pieces.next(), pieces.next()) else {
        return ArLineFamily::Malformed;
    };
    if pieces.next().is_some() {
        return ArLineFamily::Malformed;
    }
    if first.len() != 8
        || second.len() != 8
        || !first.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !second.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return ArLineFamily::Malformed;
    }
    let word = u32::from_str_radix(first, 16).unwrap_or(0);
    if word == 0 {
        return ArLineFamily::ZeroCode;
    }
    if (0x2000..0x3000).contains(&word) {
        return ArLineFamily::SelfModifying;
    }
    let subtype = (word >> 30) & 0b11;
    let code_type = (word >> 27) & 0b111;
    let size = (word >> 25) & 0b11;
    match code_type {
        0 => match subtype {
            0 => match size {
                0 => ArLineFamily::Write8,
                1 => ArLineFamily::Write16,
                2 => ArLineFamily::Write32,
                _ => ArLineFamily::WriteFloat,
            },
            1 => ArLineFamily::WritePointer,
            2 => ArLineFamily::AddCode,
            _ => ArLineFamily::MasterCode,
        },
        1..=7 => ArLineFamily::Conditional,
        _ => ArLineFamily::Malformed,
    }
}

/// Whether a `04XXXXXX` 32-bit RAM write's address fits Gecko's 24-bit
/// address field (i.e. the write lands below `0x81000000`).
fn is_gecko_addressable_write(word: u32) -> bool {
    let gcaddr = word & 0x01FF_FFFF;
    gcaddr < 0x0100_0000
}

/// A normalized, classified BSFree GameCube cheat. This is the provider-side
/// intermediate representation: provider syntax (BSFree's free-text `code`
/// and device label) is resolved here into a strict format classification and
/// canonical hex-pair lines; the Dolphin adapter then decides how those lines
/// are represented in an emulator file. Nothing in this struct touches a
/// filesystem or emulator configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeGameCubeCheat {
    pub upstream_id: i64,
    pub name: String,
    pub author: Option<String>,
    pub note: Option<String>,
    pub section: Option<String>,
    pub code_format: BsFreeGameCubeCodeFormat,
    /// Canonical uppercase `XXXXXXXX YYYY` lines, in source order.
    pub code_lines: Vec<String>,
    /// SHA-256 over the *final output form* (target section marker +
    /// canonical lines). Two cheats that would produce byte-identical
    /// emulator output share a digest, which is what output-level duplicate
    /// detection keys on.
    pub canonical_digest: String,
}

impl BsFreeGameCubeCheat {
    /// The digest of the exact lines this cheat would contribute to an
    /// emulator file, combined with the section it targets. `GeckoEquivalent`
    /// codes target `[Gecko]`; everything else targets `[ActionReplay]`.
    #[must_use]
    fn output_digest(&self) -> String {
        let mut hasher = Sha256::new();
        match self.code_format {
            BsFreeGameCubeCodeFormat::GeckoEquivalent => {
                hasher.update(b"gecko\n");
            }
            _ => hasher.update(b"actionreplay\n"),
        }
        for line in &self.code_lines {
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }
        hex_sha256(&hasher.finalize())
    }
}

/// Classifies one raw BSFree record for GameCube. The record is accepted
/// only when its device is exactly the GameCube "Action Replay" family; any
/// other device yields a `Malformed`/browse-only result with the reason kept.
pub fn classify_bsfree_gamecube_cheat(cheat: &BsFreeCheat) -> BsFreeGameCubeCheat {
    let raw_lines = cheat
        .code
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_uppercase)
        .collect::<Vec<_>>();

    let families = raw_lines
        .iter()
        .map(|line| ar_line_family(line))
        .collect::<Vec<_>>();

    let code_format = if families.is_empty()
        || families
            .iter()
            .any(|family| matches!(family, ArLineFamily::Malformed))
    {
        BsFreeGameCubeCodeFormat::Malformed
    } else if families.iter().any(|family| {
        matches!(
            family,
            ArLineFamily::MasterCode | ArLineFamily::ZeroCode | ArLineFamily::SelfModifying
        )
    }) {
        BsFreeGameCubeCodeFormat::Unsupported
    } else if families
        .iter()
        .all(|family| *family == ArLineFamily::Write32)
        && raw_lines.iter().all(|line| {
            let word = line
                .split_whitespace()
                .next()
                .and_then(|token| u32::from_str_radix(token, 16).ok())
                .unwrap_or(0);
            is_gecko_addressable_write(word)
        })
    {
        BsFreeGameCubeCodeFormat::GeckoEquivalent
    } else {
        BsFreeGameCubeCodeFormat::ActionReplayNative
    };

    let mut cheat = BsFreeGameCubeCheat {
        upstream_id: cheat.upstream_id,
        name: cheat.name.clone(),
        author: cheat.author.as_ref().map(|row| row.name.clone()),
        note: cheat.note.clone(),
        section: cheat.section.as_ref().map(|row| row.name.clone()),
        code_format,
        code_lines: raw_lines,
        canonical_digest: String::new(),
    };
    cheat.canonical_digest = cheat.output_digest();
    cheat
}

/// The stable Dolphin display name ArchiveFS uses for a BSFree-installed
/// code, matching the `"Display Name [Author]"` convention Dolphin itself
/// uses and the GameCube GameHacking adapter reproduces.
#[must_use]
pub fn bsfree_dolphin_code_name(cheat: &BsFreeGameCubeCheat) -> String {
    match cheat.author.as_deref().map(str::trim) {
        Some(author) if !author.is_empty() => format!("{} [{author}]", cheat.name),
        _ => format!("{} [{BSFREE_GAMECUBE_PROVIDER_LABEL}]", cheat.name),
    }
}

/// Maps a classified BSFree GameCube cheat to the existing GameCube
/// GameHacking adapter's input type. The adapter then routes
/// `GeckoEquivalent` → `[Gecko]` and `ActionReplayNative` →
/// `[ActionReplay]`; `Unsupported`/`Malformed` map to `RawUnknown`, which the
/// adapter's own selection logic can never select.
#[must_use]
pub fn bsfree_cheat_as_gamehacking(
    cheat: &BsFreeGameCubeCheat,
    source_game_id: u64,
) -> GameHackingGameCubeCheat {
    let code_format = match cheat.code_format {
        BsFreeGameCubeCodeFormat::GeckoEquivalent => GameCubeCodeFormat::Gecko,
        BsFreeGameCubeCodeFormat::ActionReplayNative => GameCubeCodeFormat::ActionReplay,
        BsFreeGameCubeCodeFormat::Unsupported | BsFreeGameCubeCodeFormat::Malformed => {
            GameCubeCodeFormat::RawUnknown
        }
    };
    GameHackingGameCubeCheat {
        id: format!("bsfree:{}", cheat.upstream_id),
        name: cheat.name.clone(),
        author: Some(
            cheat
                .author
                .clone()
                .unwrap_or_else(|| BSFREE_GAMECUBE_PROVIDER_LABEL.to_string()),
        ),
        description: cheat.note.clone(),
        code_format,
        code_lines: cheat.code_lines.clone(),
        // Informational only in the existing adapter's staging/merge path
        // (the destination and journal are keyed by the archive's verified
        // Dolphin Game ID, never by this provider-side number).
        source_game_id,
        source_url: BSFREE_UPSTREAM_PROJECT.to_string(),
    }
}

/// Loads and classifies every BSFree GameCube cheat for one BSFree game.
/// Read-only: this only queries the immutable BSFree catalogue.
pub fn bsfree_gamecube_cheats(
    catalogue: &BsFreeCatalogue,
    upstream_uid: i64,
) -> Result<Vec<BsFreeGameCubeCheat>, BsFreeError> {
    let page = super::PageRequest {
        offset: 0,
        limit: super::PageRequest::HARD_LIMIT,
    }
    .bounded();
    let rows = catalogue.cheats(upstream_uid, page)?;
    Ok(rows
        .rows
        .into_iter()
        .map(|cheat| classify_bsfree_gamecube_cheat(&cheat))
        .collect())
}

/// Typed duplicate/conflict finding from the two-pass analysis described in
/// the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeDedupFindingKind {
    /// The exact same record (name + canonical body) appears more than once
    /// in the BSFree game's own catalogue.
    DuplicateRecord,
    /// Two different BSFree records share the same canonical code body
    /// before any conversion (different labels, identical code).
    DuplicateBody,
    /// Two different BSFree records share the same name but differ in body.
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
    /// The cheat is not a well-formed/installable format at all.
    NotInstallable,
}

impl BsFreeDedupFindingKind {
    #[must_use]
    pub const fn blocks_selection(self) -> bool {
        matches!(
            self,
            Self::DuplicateNameConflict
                | Self::CrossSectionCollision
                | Self::SameLabelDifferentBody
                | Self::NotInstallable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeDedupFinding {
    pub kind: BsFreeDedupFindingKind,
    pub cheat_upstream_id: i64,
    pub cheat_name: String,
    pub relates_to: Option<String>,
    pub detail: String,
}

/// Two-pass duplicate/conflict analysis for a set of classified BSFree
/// GameCube cheats against an existing Dolphin GameSettings document.
///
/// Pass A (source-level) runs over the BSFree records alone: duplicate
/// records, duplicate bodies, and same-name-different-body records.
///
/// Pass B (output-level) runs over the *final output form* produced by any
/// classification: two records converting to identical Gecko/AR output, a
/// converted result colliding with an already-installed user/ArchiveFS cheat
/// in the same or the other section, and same-name-different-body collisions
/// with installed content.
pub fn analyze_bsfree_gamecube_duplicates<'a>(
    cheats: impl IntoIterator<Item = &'a BsFreeGameCubeCheat>,
    destination: &DolphinIniDocument,
) -> Vec<BsFreeDedupFinding> {
    let cheats = cheats.into_iter().collect::<Vec<_>>();
    let mut findings = Vec::new();

    // Pass A: source-level duplicates.
    let mut by_record: std::collections::BTreeMap<(String, String), Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut by_body: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut by_name: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, cheat) in cheats.iter().enumerate() {
        by_record
            .entry((cheat.name.clone(), cheat.canonical_digest.clone()))
            .or_default()
            .push(index);
        by_body
            .entry(cheat.canonical_digest.clone())
            .or_default()
            .push(index);
        by_name.entry(cheat.name.clone()).or_default().push(index);
    }
    for indices in by_record.values() {
        for (offset, index) in indices.iter().skip(1).enumerate() {
            let cheat = &cheats[*index];
            findings.push(BsFreeDedupFinding {
                kind: BsFreeDedupFindingKind::DuplicateRecord,
                cheat_upstream_id: cheat.upstream_id,
                cheat_name: cheat.name.clone(),
                relates_to: indices.first().map(|i| cheats[*i].upstream_id.to_string()),
                detail: format!(
                    "the exact same BSFree record appears again (occurrence {}); only the first is ever installed",
                    offset + 2
                ),
            });
        }
    }
    for (digest, indices) in &by_body {
        if indices.len() > 1 {
            for index in indices.iter().skip(1) {
                let cheat = &cheats[*index];
                findings.push(BsFreeDedupFinding {
                    kind: BsFreeDedupFindingKind::DuplicateBody,
                    cheat_upstream_id: cheat.upstream_id,
                    cheat_name: cheat.name.clone(),
                    relates_to: indices
                        .first()
                        .map(|i| format!("{} ({})", cheats[*i].name, cheats[*i].upstream_id)),
                    detail: "a different BSFree label carries the same code body; the labels are variants, not independent cheats"
                        .to_string(),
                });
            }
        }
        let _ = digest;
    }
    for (name, indices) in &by_name {
        let mut bodies = indices
            .iter()
            .map(|i| cheats[*i].canonical_digest.clone())
            .collect::<Vec<_>>();
        bodies.sort();
        bodies.dedup();
        if bodies.len() > 1 {
            for index in indices {
                let cheat = &cheats[*index];
                findings.push(BsFreeDedupFinding {
                    kind: BsFreeDedupFindingKind::DuplicateNameConflict,
                    cheat_upstream_id: cheat.upstream_id,
                    cheat_name: cheat.name.clone(),
                    relates_to: None,
                    detail: format!(
                        "the BSFree game contains multiple cheats named {:?} with different bodies; \
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
        let target_gecko = cheat.code_format == BsFreeGameCubeCodeFormat::GeckoEquivalent;
        let (same_section_bodies, other_section_bodies) = if target_gecko {
            (&installed_gecko, &installed_ar)
        } else {
            (&installed_ar, &installed_gecko)
        };
        let dolphin_name = bsfree_dolphin_code_name(cheat);

        if let Some((existing_name, _)) = same_section_bodies
            .iter()
            .find(|(_, lines)| **lines == cheat.code_lines)
        {
            let kind = if *existing_name == dolphin_name {
                BsFreeDedupFindingKind::AlreadyInstalled
            } else {
                BsFreeDedupFindingKind::AlreadyInstalledDifferentName
            };
            findings.push(BsFreeDedupFinding {
                kind,
                cheat_upstream_id: cheat.upstream_id,
                cheat_name: cheat.name.clone(),
                relates_to: Some(existing_name.clone()),
                detail: if *existing_name == dolphin_name {
                    "the same code is already installed under this name; re-installing is a no-op".to_string()
                } else {
                    format!("an identical code is already installed under {:?}; it will not be installed a second time", existing_name)
                },
            });
            continue;
        }

        if let Some((existing_name, _)) = other_section_bodies
            .iter()
            .find(|(_, lines)| **lines == cheat.code_lines)
        {
            findings.push(BsFreeDedupFinding {
                kind: BsFreeDedupFindingKind::CrossSectionCollision,
                cheat_upstream_id: cheat.upstream_id,
                cheat_name: cheat.name.clone(),
                relates_to: Some(existing_name.clone()),
                detail: format!(
                    "the same hex-pair body is already installed in the other Dolphin section under {:?}; \
                     the two engines interpret these bytes differently, so this is reported for review and not applied",
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
            findings.push(BsFreeDedupFinding {
                kind: BsFreeDedupFindingKind::SameLabelDifferentBody,
                cheat_upstream_id: cheat.upstream_id,
                cheat_name: cheat.name.clone(),
                relates_to: Some(dolphin_name.clone()),
                detail: format!(
                    "a code named {:?} is already installed (in {} the {}) with a different body; \
                     ArchiveFS will not overwrite it",
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

/// A BSFree GameCube cheat selection that reuses the existing
/// `GameCubeCheatSelection` semantics but tracks the original BSFree record
/// and its dedup findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeGameCubeSelectionEntry {
    pub index: usize,
    pub upstream_id: i64,
    pub name: String,
    pub code_format: BsFreeGameCubeCodeFormat,
    /// `true` only for installable formats with at least one well-formed
    /// hex-pair line. `Unsupported`/`Malformed` can never become `true`.
    pub selectable: bool,
    pub selected: bool,
    pub already_managed: bool,
    pub dolphin_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeGameCubeCheatSelection {
    pub entries: Vec<BsFreeGameCubeSelectionEntry>,
}

impl BsFreeGameCubeCheatSelection {
    #[must_use]
    pub fn from_cheats(cheats: &[BsFreeGameCubeCheat], destination: &DolphinIniDocument) -> Self {
        let managed = super::gamehacking_gamecube_install_plan::managed_names(destination);
        let entries = cheats
            .iter()
            .enumerate()
            .map(|(index, cheat)| {
                let selectable = cheat.code_format.is_installable()
                    && !cheat.code_lines.is_empty()
                    && cheat.code_lines.iter().all(|line| is_gecko_code_line(line));
                let dolphin_name = bsfree_dolphin_code_name(cheat);
                BsFreeGameCubeSelectionEntry {
                    index,
                    upstream_id: cheat.upstream_id,
                    name: cheat.name.clone(),
                    code_format: cheat.code_format,
                    selectable,
                    selected: false,
                    already_managed: managed.contains(&dolphin_name),
                    dolphin_name,
                }
            })
            .collect();
        Self { entries }
    }

    #[must_use]
    pub fn selected_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.selected).count()
    }

    #[must_use]
    pub fn selectable_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.selectable).count()
    }

    #[must_use]
    pub fn can_apply(&self) -> bool {
        self.selected_count() > 0
    }

    /// Ticks one entry. Returns `false` - changing nothing - for an unknown
    /// or unselectable entry, so an `Unsupported`/`Malformed` cheat can never
    /// become selected.
    pub fn set_selected(&mut self, index: usize, selected: bool) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.index == index) else {
            return false;
        };
        if selected && !entry.selectable {
            return false;
        }
        entry.selected = selected;
        true
    }

    pub fn select_all(&mut self) {
        for entry in &mut self.entries {
            if entry.selectable {
                entry.selected = true;
            }
        }
    }

    pub fn clear_all(&mut self) {
        for entry in &mut self.entries {
            entry.selected = false;
        }
    }

    /// The selected cheats, re-validated against the caller's own classified
    /// list and re-checked for format eligibility - never trusts a stale
    /// `index`/`selectable` flag alone.
    pub fn resolve<'a>(
        &self,
        cheats: &'a [BsFreeGameCubeCheat],
    ) -> Result<Vec<&'a BsFreeGameCubeCheat>, BsFreeGameCubeError> {
        let mut selected = Vec::new();
        for row in self.entries.iter().filter(|entry| entry.selected) {
            let cheat = cheats.get(row.index).ok_or_else(|| {
                BsFreeGameCubeError::new(
                    BsFreeGameCubeErrorKind::SelectionInvalid,
                    Some(row.name.clone()),
                    "selected cheat is no longer in the fetched list",
                )
            })?;
            if cheat.upstream_id != row.upstream_id || !row.selectable {
                return Err(BsFreeGameCubeError::new(
                    BsFreeGameCubeErrorKind::SelectionInvalid,
                    Some(cheat.name.clone()),
                    "selected cheat is not safe to install",
                ));
            }
            if !cheat.code_format.is_installable() {
                return Err(BsFreeGameCubeError::new(
                    BsFreeGameCubeErrorKind::UnsupportedFormat,
                    Some(cheat.name.clone()),
                    format!(
                        "{:?} cheats are browse-only and can never be installed",
                        cheat.code_format
                    ),
                ));
            }
            selected.push(cheat);
        }
        if selected.is_empty() {
            return Err(BsFreeGameCubeError::new(
                BsFreeGameCubeErrorKind::NoSelectedCheats,
                None,
                "no installable Action Replay or Gecko cheats are selected; choose at least one before installing",
            ));
        }
        Ok(selected)
    }
}

/// Result of staging a BSFree GameCube install: the staged artifact plus the
/// full dedup/conflict analysis and the lists of cheats that were skipped
/// (rather than staged).
#[derive(Debug, Clone)]
pub struct BsFreeStagedGameCubeInstall {
    pub staged: StagedGameCubeIni,
    pub findings: Vec<BsFreeDedupFinding>,
    /// Installable cheats that were *not* staged because an output-level
    /// duplicate already exists in the destination or in the selection.
    pub skipped_duplicates: Vec<String>,
    /// Unsupported/malformed cheats in the full fetched list, skipped
    /// regardless of selection.
    pub skipped_unselectable: Vec<String>,
}

/// Error type for the BSFree GameCube bridge. Staging errors are reused
/// verbatim from the existing GameCube GameHacking adapter via
/// [`BsFreeGameCubeError::staging`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeGameCubeErrorKind {
    NoSelectedCheats,
    SelectionInvalid,
    UnsupportedFormat,
    Staging(GameCubeInstallPlanErrorKind),
    PreviewFailed,
    ConflictingSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeGameCubeError {
    pub kind: BsFreeGameCubeErrorKind,
    pub cheat_name: Option<String>,
    pub detail: String,
}

impl BsFreeGameCubeError {
    fn new(
        kind: BsFreeGameCubeErrorKind,
        cheat_name: Option<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            cheat_name,
            detail: detail.into(),
        }
    }

    fn staging(error: GameCubeInstallPlanError) -> Self {
        let cheat_name = error.cheat_name;
        Self {
            kind: BsFreeGameCubeErrorKind::Staging(error.kind),
            cheat_name,
            detail: error.detail,
        }
    }

    fn preview(error: GameCubeInstallPlanError) -> Self {
        let cheat_name = error.cheat_name;
        Self {
            kind: BsFreeGameCubeErrorKind::PreviewFailed,
            cheat_name,
            detail: error.detail,
        }
    }
}

impl std::fmt::Display for BsFreeGameCubeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for BsFreeGameCubeError {}

/// Stages the selected BSFree GameCube cheats for install through the
/// existing GameCube GameHacking adapter.
///
/// The provider (this module) supplies classified cheat data. The adapter
/// (`stage_gamecube_gamehacking_install`) decides the emulator representation
/// (`[Gecko]`/`[ActionReplay]` + `_Enabled` + the managed bookkeeping
/// section). The shared apply pipeline performs the actual mutation. Nothing
/// here writes an emulator file directly.
///
/// Deduplication is applied *before* staging: output-level duplicates within
/// the selection and against the destination are reported and skipped; the
/// existing adapter's own name-conflict rules then act as a second layer.
pub fn stage_bsfree_gamecube_install(
    staging_root: &std::path::Path,
    file_name: &str,
    destination: &DolphinIniDocument,
    destination_existed: bool,
    all_cheats: &[BsFreeGameCubeCheat],
    selection: &BsFreeGameCubeCheatSelection,
) -> Result<BsFreeStagedGameCubeInstall, BsFreeGameCubeError> {
    let selected = selection.resolve(all_cheats)?;

    let findings = analyze_bsfree_gamecube_duplicates(selected.iter().copied(), destination);

    // Blocking findings prevent staging entirely for the affected cheat.
    let mut staged_cheats = Vec::new();
    let mut skipped_duplicates = Vec::new();
    let mut seen_outputs = std::collections::BTreeSet::new();
    for cheat in selected {
        let blocking = findings
            .iter()
            .filter(|finding| finding.cheat_upstream_id == cheat.upstream_id)
            .filter(|finding| finding.kind.blocks_selection())
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            return Err(BsFreeGameCubeError::new(
                BsFreeGameCubeErrorKind::ConflictingSelection,
                Some(cheat.name.clone()),
                format!(
                    "cheat {:?} conflicts with installed content and was not staged: {}",
                    cheat.name,
                    blocking
                        .first()
                        .map(|finding| finding.detail.as_str())
                        .unwrap_or_default()
                ),
            ));
        }
        // The exact same output body already exists in the destination under a
        // *different* name (same section): never install it a second time.
        let already_covered_different_name = findings.iter().any(|finding| {
            finding.cheat_upstream_id == cheat.upstream_id
                && finding.kind == BsFreeDedupFindingKind::AlreadyInstalledDifferentName
        });
        if already_covered_different_name {
            skipped_duplicates.push(cheat.name.clone());
            continue;
        }
        let output_digest = cheat.output_digest();
        if !seen_outputs.insert(output_digest) {
            skipped_duplicates.push(cheat.name.clone());
            continue;
        }
        staged_cheats.push(cheat);
    }

    if staged_cheats.is_empty() {
        return Err(BsFreeGameCubeError::new(
            BsFreeGameCubeErrorKind::NoSelectedCheats,
            None,
            "all selected cheats were duplicates already covered by the destination or the selection",
        ));
    }

    let adapter_cheats = staged_cheats
        .iter()
        .map(|cheat| bsfree_cheat_as_gamehacking(cheat, 0))
        .collect::<Vec<_>>();
    let mut adapter_selection = GameCubeCheatSelection::from_cheats(&adapter_cheats, destination);
    if adapter_selection.selected_count() == 0 {
        adapter_selection.select_all();
    }

    let skipped_unselectable = all_cheats
        .iter()
        .filter(|cheat| !cheat.code_format.is_installable())
        .map(|cheat| cheat.name.clone())
        .collect::<Vec<_>>();

    let staged = stage_gamecube_gamehacking_install(
        staging_root,
        file_name,
        destination,
        destination_existed,
        &adapter_cheats,
        &adapter_selection,
    )
    .map_err(BsFreeGameCubeError::staging)?;

    Ok(BsFreeStagedGameCubeInstall {
        staged,
        findings,
        skipped_duplicates,
        skipped_unselectable,
    })
}

/// Request for a BSFree GameCube install preview. Mirrors
/// [`GameCubeGameHackingInstallPreviewRequest`] exactly; the selected
/// archive's verified Dolphin Game ID is the identity the shared preview is
/// bound to.
#[derive(Debug, Clone)]
pub struct BsFreeGameCubeInstallPreviewRequest {
    pub selected_archive: PathBuf,
    /// The Dolphin profile's own configuration root - matches the real
    /// `<configuration_path>/GameSettings/<GameID>.ini` layout exactly.
    pub configuration_path: PathBuf,
    pub game_id: String,
    pub revision: Option<u16>,
    pub staged: StagedGameCubeIni,
}

/// The preview returned by [`build_bsfree_gamecube_install_preview`] - the
/// same type the existing GameCube GameHacking adapter returns, since both go
/// through the identical shared preview boundary.
pub type BsFreeGameCubeInstallPreview = GameCubeGameHackingInstallPreview;

/// Builds the shared read-only preview for a staged BSFree GameCube install,
/// by delegating to the exact preview boundary the existing GameCube
/// GameHacking adapter uses. Always `VerifiedExact` on the selected archive's
/// verified Dolphin Game ID; an ambiguous or missing match never reaches here.
pub fn build_bsfree_gamecube_install_preview(
    request: &BsFreeGameCubeInstallPreviewRequest,
) -> Result<GameCubeGameHackingInstallPreview, BsFreeGameCubeError> {
    build_dolphin_gamehacking_install_preview(
        &GameCubeGameHackingInstallPreviewRequest {
            selected_archive: request.selected_archive.clone(),
            configuration_path: request.configuration_path.clone(),
            game_id: request.game_id.clone(),
            revision: request.revision,
            staged: request.staged.clone(),
        },
        "GameCube",
    )
    .map_err(BsFreeGameCubeError::preview)
}

/// Result of matching a BSFree GameCube game against a selected archive's
/// verified identity. BSFree has no emulator-stable identifiers, so the
/// strongest achievable evidence is platform + normalized title + compatible
/// region/version - which always requires explicit user confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeGameCubeMatch {
    pub archive_title: String,
    pub archive_game_id: String,
    pub matched_bsfree_game_upstream_uid: i64,
    pub matched_bsfree_title: String,
    pub matched_bsfree_version: Option<String>,
    pub region_evidence: String,
    pub requires_review: bool,
    pub detail: String,
}

/// Matches a selected archive (with its verified Dolphin Game ID and title)
/// to exactly one BSFree GameCube game, requiring normalized title equality
/// and a compatible region where the archive can supply one. Returns `None`
/// when no candidate is strong enough - the caller then shows candidates for
/// review rather than applying.
pub fn bsfree_gamecube_match(
    catalogue: &BsFreeCatalogue,
    archive_title: &str,
    archive_game_id: &str,
    archive_region: Option<&str>,
) -> Result<Option<BsFreeGameCubeMatch>, BsFreeError> {
    let normalized_title = normalize_title(archive_title);
    let search = catalogue.search_games(&super::BsFreeGameSearchRequest {
        platform_id: Some("GameCube".to_string()),
        title: archive_title.to_string(),
        version: None,
        device_id: None,
        upstream_game_id: None,
        page: super::PageRequest::games(0).bounded(),
    })?;
    let mut best: Option<BsFreeGameCubeMatch> = None;
    let mut tied = false;
    for game in search.page.rows {
        let game_title_normalized = normalize_title(&game.name);
        if game_title_normalized.is_empty() || game_title_normalized != normalized_title {
            continue;
        }
        let region_evidence = region_evidence(game.version.as_deref(), archive_region);
        let candidate = BsFreeGameCubeMatch {
            archive_title: archive_title.to_string(),
            archive_game_id: archive_game_id.to_string(),
            matched_bsfree_game_upstream_uid: game.upstream_uid,
            matched_bsfree_title: game.name.clone(),
            matched_bsfree_version: game.version.clone(),
            region_evidence,
            requires_review: true,
            detail: "matched by platform and exact normalized title; BSFree carries no \
                     emulator-stable identifier, so this match always requires review"
                .to_string(),
        };
        match &best {
            None => best = Some(candidate),
            Some(previous)
                if previous.matched_bsfree_game_upstream_uid
                    != candidate.matched_bsfree_game_upstream_uid =>
            {
                tied = true;
            }
            Some(_) => {}
        }
    }
    if tied {
        // Multiple distinct BSFree games match the same normalized title:
        // ambiguous, do not select one silently.
        return Ok(None);
    }
    Ok(best)
}

fn region_evidence(bsfree_version: Option<&str>, archive_region: Option<&str>) -> String {
    match (bsfree_version, archive_region) {
        (Some(version), Some(region))
            if version
                .to_ascii_uppercase()
                .contains(&region.to_ascii_uppercase()) =>
        {
            format!("BSFree version {version:?} contains the archive's region {region:?}")
        }
        (Some(version), Some(region)) => format!(
            "BSFree version {version:?} does not explicitly state the archive's region {region:?}"
        ),
        (Some(version), None) => format!("BSFree version {version:?}; archive region unknown"),
        (None, Some(region)) => {
            format!("BSFree has no version/region field; archive region is {region:?}")
        }
        (None, None) => "no region or version evidence on either side".to_string(),
    }
}

fn normalize_title(value: &str) -> String {
    // Strip parenthesized/bracketed region, edition and revision markers
    // ("(USA)", "[Europe]", "(Rev 1)") before comparing, so an archive title
    // carrying a region suffix still matches the bare BSFree title without
    // ever weakening an exact normalized-title equality below that.
    let mut without_markers = String::with_capacity(value.len());
    let mut depth = 0u8;
    for character in value.chars() {
        match character {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => without_markers.push(character),
            _ => {}
        }
    }
    without_markers
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn hex_sha256(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;
