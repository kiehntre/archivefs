//! BSFree Wii → Dolphin adapter bridge.
//!
//! This mirrors `bsfree_gamecube.rs` exactly for the Wii platform: raw BSFree
//! Archive records become installable Dolphin GameSettings content only when
//! they classify into the *same* byte-identity subset that is already trusted
//! for the GameHacking Wii/Dolphin path. It is deliberately a thin,
//! conservative seam on top of the existing Wii GameHacking install adapter
//! ([`stage_wii_gamehacking_install`]) and the shared preview/apply/rollback
//! transaction pipeline - it never writes an emulator file directly and never
//! invents a new conversion.
//!
//! # What is safe to install (the verified Wii subset)
//!
//! A BSFree Wii code is offered for installation only when ALL of these hold:
//!
//! - the record's declared device is the proven Action Replay family or a
//!   native Gecko device (the same explicit-format gate the GameHacking Wii
//!   provider enforces with `WiiCheatSafety::UnverifiedFormatLabel`);
//! - every line is a strict `XXXXXXXX YYYY` hex-pair line with no placeholders;
//! - no master/enable-code dependency (checked in the name, matching
//!   `classify_wii_cheat_safety`);
//! - no Action Replay command Dolphin refuses at runtime
//!   (master/zero/self-modifying codes).
//!
//! Within that subset, `GeckoEquivalent` lines (every line an AR 32-bit RAM
//! write whose address fits Gecko's 24-bit field) are emitted into `[Gecko]`
//! unchanged - the byte-identity proof in `dolphin_code.rs` is shared with
//! GameCube because both platforms use Dolphin's same `ActionReplay.cpp`.
//! Every other well-formed hex-pair code is emitted verbatim into
//! `[ActionReplay]`, exactly like the GameCube adapter.
//!
//! Everything else - encrypted dash-format codes, placeholders, unknown
//! devices, malformed lines - remains browse-only. EmuWiz has no verified
//! GameCube/Wii dash-encrypted Action Replay decryptor, so those records are
//! never "fixed up" into an installable form.
//!
//! # Identity is the selected archive's, never BSFree's
//!
//! The destination is keyed by the selected game archive's *verified* Dolphin
//! Wii Game ID (`WiiGameIdentity`), exactly like the GameHacking Wii adapter.
//! BSFree contributes only platform + normalized title + version/region
//! evidence, which requires explicit user confirmation before Apply.
//!
//! # Data availability
//!
//! The shipped BSFree catalogue snapshot contains no Wii system rows (see
//! `docs/BSFREE_GAMECUBE_CHEAT_APPLY.md`), so `bsfree_wii_search` returns
//! `NoMatch` on it. The classifier, selection, staging and rollback paths
//! below are implemented and tested with representative fixtures so the
//! pipeline activates automatically - and conservatively - if a database
//! containing Wii rows is ever loaded.

use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::ReadOnlyCheatCatalogue;
use super::bsfree::{BSFREE_UPSTREAM_PROJECT, BsFreeCatalogue, BsFreeCheat, BsFreeError};
use super::dolphin_code::{
    ArLineFamily, MemoryOperation, ar_line_family, contains_placeholder, derive_memory_operations,
    hex_sha256, is_gecko_addressable_write, strict_code_line,
};
use super::dolphin_dedup::{
    DolphinCheat, DolphinDedupFinding, DolphinDedupFindingKind, analyze_dolphin_duplicates,
};
use super::gamehacking_gamecube_install_plan::{
    GameCubeGameHackingInstallPreview, GameCubeGameHackingInstallPreviewRequest,
    GameCubeInstallPlanError, GameCubeInstallPlanErrorKind, StagedGameCubeIni,
    build_dolphin_gamehacking_install_preview,
};
use super::gamehacking_wii_provider::{
    GameHackingWiiCheat, WiiCheatSafety, WiiCodeFormat, stage_wii_gamehacking_install,
};
use super::gecko_document::{DolphinIniDocument, is_gecko_code_line};

pub const BSFREE_WII_PROVIDER_LABEL: &str = "BSFree Archive";

/// The BSFree device names this build treats as an explicit, verified format
/// label for Wii - the same gate `WiiCheatSafety::UnverifiedFormatLabel`
/// enforces for the GameHacking Wii provider. Anything else is browse-only.
fn device_is_verified_wii_format(device_name: &str) -> bool {
    let name = device_name.trim().to_ascii_lowercase();
    matches!(name.as_str(), "action replay" | "gecko")
}

/// One BSFree Wii cheat's classification. The grammar and the byte-identity
/// proof are shared with GameCube (`dolphin_code.rs`); the enum mirrors
/// `BsFreeGameCubeCodeFormat` because both platforms target the same Dolphin
/// `GameSettings` structure with the same engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeWiiCodeFormat {
    /// Every line is an Action Replay 32-bit RAM write whose address fits
    /// Gecko's 24-bit field; Dolphin executes the identical bytes the same
    /// way under `[Gecko]`, so this is installed as native Gecko with no
    /// transformation.
    GeckoEquivalent,
    /// A well-formed hex-pair Action Replay code, installed verbatim into
    /// `[ActionReplay]`. Never converted.
    ActionReplayNative,
    /// Well-formed hex pairs containing an Action Replay command Dolphin
    /// refuses at runtime (master/zero/self-modifying codes).
    Unsupported,
    /// Not a well-formed `XXXXXXXX YYYY` hex-pair code (placeholders,
    /// encrypted dash-format codes, free text, or an unverified device).
    Malformed,
}

impl BsFreeWiiCodeFormat {
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
                "A well-formed Wii Action Replay hex-pair code. Installed verbatim into \
                 Dolphin's [ActionReplay] section; it is not relabelled as Gecko."
            }
            Self::Unsupported => {
                "The code body contains an Action Replay command Dolphin refuses to run \
                 (a master code, zero code, or self-modifying code). Browse-only."
            }
            Self::Malformed => {
                "The code body is not a well-formed X XXXXXXXX YYYYYYYY hex-pair code, or \
                 its device is not a verified Wii format (placeholders, encrypted codes, \
                 free text, or unknown devices stay browse-only)."
            }
        }
    }
}

/// A normalized, classified BSFree Wii cheat - the provider-side intermediate
/// representation for the Wii/Dolphin path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeWiiCheat {
    pub upstream_id: i64,
    pub name: String,
    pub author: Option<String>,
    pub note: Option<String>,
    pub section: Option<String>,
    pub code_format: BsFreeWiiCodeFormat,
    /// Canonical uppercase `XXXXXXXX YYYY` lines, in source order.
    pub code_lines: Vec<String>,
    /// SHA-256 over the *final output form* (target section marker +
    /// canonical lines). Two cheats that would produce byte-identical
    /// emulator output share a digest.
    pub canonical_digest: String,
}

impl BsFreeWiiCheat {
    /// The digest of the exact lines this cheat would contribute to a Dolphin
    /// file, combined with the section it targets (`[Gecko]` vs
    /// `[ActionReplay]`). Shared with the generalized analyser's fingerprint.
    #[must_use]
    pub fn output_digest(&self) -> String {
        let mut hasher = Sha256::new();
        match self.code_format {
            BsFreeWiiCodeFormat::GeckoEquivalent => {
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

/// The normalized Dolphin view for the shared duplicate/conflict analyser.
impl DolphinCheat for BsFreeWiiCheat {
    fn upstream_id(&self) -> i64 {
        self.upstream_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn dolphin_name(&self) -> String {
        bsfree_dolphin_wii_code_name(self)
    }

    fn code_lines(&self) -> &[String] {
        &self.code_lines
    }

    fn target_gecko(&self) -> bool {
        self.code_format == BsFreeWiiCodeFormat::GeckoEquivalent
    }

    fn output_digest(&self) -> String {
        self.output_digest()
    }

    fn installable(&self) -> bool {
        self.code_format.is_installable()
    }

    fn memory_operations(&self) -> Vec<MemoryOperation> {
        derive_memory_operations(&self.code_lines)
    }
}

/// Classifies one raw BSFree record for Wii.
///
/// The record is accepted only when its declared device is a verified Wii
/// format (Action Replay or Gecko) AND its body is a strict hex-pair set with
/// no master/self-modifying/zero codes and no placeholders. Anything else is
/// `Malformed`/`Unsupported` and stays browse-only.
pub fn classify_bsfree_wii_cheat(cheat: &BsFreeCheat) -> BsFreeWiiCheat {
    let device_name = cheat.device.name.as_str();
    if !device_is_verified_wii_format(device_name) {
        return BsFreeWiiCheat {
            upstream_id: cheat.upstream_id,
            name: cheat.name.clone(),
            author: cheat.author.as_ref().map(|row| row.name.clone()),
            note: cheat.note.clone(),
            section: cheat.section.as_ref().map(|row| row.name.clone()),
            code_format: BsFreeWiiCodeFormat::Malformed,
            code_lines: Vec::new(),
            canonical_digest: String::new(),
        };
    }

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

    let name_has_master_requirement = {
        let lower = cheat.name.to_ascii_lowercase();
        lower.contains("master code") || lower.contains("enable code")
    };

    let code_format = if raw_lines.is_empty()
        || raw_lines.iter().any(|line| contains_placeholder(line))
        || families
            .iter()
            .any(|family| matches!(family, ArLineFamily::Malformed))
        || !raw_lines.iter().all(|line| strict_code_line(line))
    {
        BsFreeWiiCodeFormat::Malformed
    } else if name_has_master_requirement
        || families.iter().any(|family| {
            matches!(
                family,
                ArLineFamily::MasterCode | ArLineFamily::ZeroCode | ArLineFamily::SelfModifying
            )
        })
    {
        BsFreeWiiCodeFormat::Unsupported
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
        BsFreeWiiCodeFormat::GeckoEquivalent
    } else {
        BsFreeWiiCodeFormat::ActionReplayNative
    };

    let mut cheat = BsFreeWiiCheat {
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

/// The stable Dolphin display name EmuWiz uses for a BSFree-installed Wii
/// code, matching the `"Display Name [Author]"` convention.
#[must_use]
pub fn bsfree_dolphin_wii_code_name(cheat: &BsFreeWiiCheat) -> String {
    match cheat.author.as_deref().map(str::trim) {
        Some(author) if !author.is_empty() => format!("{} [{author}]", cheat.name),
        _ => format!("{} [{BSFREE_WII_PROVIDER_LABEL}]", cheat.name),
    }
}

/// Maps a classified BSFree Wii cheat to the existing Wii GameHacking
/// adapter's input type, carrying `WiiCodeFormat` + `WiiCheatSafety` so the
/// shared adapter's own gates apply. `Unsupported`/`Malformed` map to
/// `Unsupported`/`RawUnknown` with a non-installable safety, which the
/// adapter can never select.
#[must_use]
pub fn bsfree_cheat_as_wii(cheat: &BsFreeWiiCheat, source_game_id: u64) -> GameHackingWiiCheat {
    let (code_format, safety) = match cheat.code_format {
        BsFreeWiiCodeFormat::GeckoEquivalent => (WiiCodeFormat::Gecko, WiiCheatSafety::Installable),
        BsFreeWiiCodeFormat::ActionReplayNative => {
            (WiiCodeFormat::ActionReplay, WiiCheatSafety::Installable)
        }
        BsFreeWiiCodeFormat::Unsupported => {
            (WiiCodeFormat::Unsupported, WiiCheatSafety::MalformedCode)
        }
        BsFreeWiiCodeFormat::Malformed => (
            WiiCodeFormat::RawUnknown,
            WiiCheatSafety::UnverifiedFormatLabel,
        ),
    };
    GameHackingWiiCheat {
        id: format!("bsfree-wii:{}", cheat.upstream_id),
        name: cheat.name.clone(),
        author: Some(
            cheat
                .author
                .clone()
                .unwrap_or_else(|| BSFREE_WII_PROVIDER_LABEL.to_string()),
        ),
        description: cheat.note.clone(),
        code_format,
        safety,
        safety_warnings: Vec::new(),
        code_lines: cheat.code_lines.clone(),
        source_game_id,
        source_url: BSFREE_UPSTREAM_PROJECT.to_string(),
    }
}

/// Loads and classifies every BSFree Wii cheat for one BSFree game.
/// Read-only: this only queries the immutable BSFree catalogue.
pub fn bsfree_wii_cheats(
    catalogue: &BsFreeCatalogue,
    upstream_uid: i64,
) -> Result<Vec<BsFreeWiiCheat>, BsFreeError> {
    let page = super::PageRequest {
        offset: 0,
        limit: super::PageRequest::HARD_LIMIT,
    }
    .bounded();
    let rows = catalogue.cheats(upstream_uid, page)?;
    Ok(rows
        .rows
        .into_iter()
        .map(|cheat| classify_bsfree_wii_cheat(&cheat))
        .collect())
}

/// Wii duplicate/conflict finding - the shared [`DolphinDedupFinding`] with
/// BSFree Wii provenance, so GameCube and Wii (and cross-provider) analyses
/// all speak the same finding vocabulary.
pub type BsFreeWiiDedupFinding = DolphinDedupFinding;
pub type BsFreeWiiDedupFindingKind = DolphinDedupFindingKind;

/// Two-pass duplicate/conflict analysis for a set of classified BSFree Wii
/// cheats against an existing Dolphin GameSettings document. Delegates to the
/// shared, platform-parameterized analyser.
pub fn analyze_bsfree_wii_duplicates<'a>(
    cheats: impl IntoIterator<Item = &'a BsFreeWiiCheat>,
    destination: &DolphinIniDocument,
) -> Vec<BsFreeWiiDedupFinding> {
    let views: Vec<&dyn DolphinCheat> = cheats
        .into_iter()
        .map(|cheat| cheat as &dyn DolphinCheat)
        .collect();
    analyze_dolphin_duplicates(&views, destination)
}

/// A BSFree Wii cheat selection that reuses the existing adapter selection
/// semantics but tracks the original BSFree record and its dedup findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeWiiSelectionEntry {
    pub index: usize,
    pub upstream_id: i64,
    pub name: String,
    pub code_format: BsFreeWiiCodeFormat,
    /// `true` only for installable formats with at least one well-formed
    /// hex-pair line. `Unsupported`/`Malformed` can never become `true`.
    pub selectable: bool,
    pub selected: bool,
    pub already_managed: bool,
    pub dolphin_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeWiiCheatSelection {
    pub entries: Vec<BsFreeWiiSelectionEntry>,
}

impl BsFreeWiiCheatSelection {
    #[must_use]
    pub fn from_cheats(cheats: &[BsFreeWiiCheat], destination: &DolphinIniDocument) -> Self {
        let managed = super::gamehacking_gamecube_install_plan::managed_names(destination);
        let entries = cheats
            .iter()
            .enumerate()
            .map(|(index, cheat)| {
                let selectable = cheat.code_format.is_installable()
                    && !cheat.code_lines.is_empty()
                    && cheat.code_lines.iter().all(|line| is_gecko_code_line(line));
                let dolphin_name = bsfree_dolphin_wii_code_name(cheat);
                BsFreeWiiSelectionEntry {
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
        cheats: &'a [BsFreeWiiCheat],
    ) -> Result<Vec<&'a BsFreeWiiCheat>, BsFreeWiiError> {
        let mut selected = Vec::new();
        for row in self.entries.iter().filter(|entry| entry.selected) {
            let cheat = cheats.get(row.index).ok_or_else(|| {
                BsFreeWiiError::new(
                    BsFreeWiiErrorKind::SelectionInvalid,
                    Some(row.name.clone()),
                    "selected cheat is no longer in the fetched list",
                )
            })?;
            if cheat.upstream_id != row.upstream_id || !row.selectable {
                return Err(BsFreeWiiError::new(
                    BsFreeWiiErrorKind::SelectionInvalid,
                    Some(cheat.name.clone()),
                    "selected cheat is not safe to install",
                ));
            }
            if !cheat.code_format.is_installable() {
                return Err(BsFreeWiiError::new(
                    BsFreeWiiErrorKind::UnsupportedFormat,
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
            return Err(BsFreeWiiError::new(
                BsFreeWiiErrorKind::NoSelectedCheats,
                None,
                "no installable Action Replay or Gecko cheats are selected; choose at least \
                 one before installing",
            ));
        }
        Ok(selected)
    }
}

/// Result of staging a BSFree Wii install: the staged artifact plus the full
/// dedup/conflict analysis and the cheats that were skipped.
#[derive(Debug, Clone)]
pub struct BsFreeStagedWiiInstall {
    pub staged: StagedGameCubeIni,
    pub findings: Vec<BsFreeWiiDedupFinding>,
    /// Installable cheats that were *not* staged because an output-level
    /// duplicate already exists in the destination or in the selection.
    pub skipped_duplicates: Vec<String>,
    /// Unsupported/malformed cheats in the full fetched list, skipped
    /// regardless of selection.
    pub skipped_unselectable: Vec<String>,
}

/// Error type for the BSFree Wii bridge. Staging errors are reused verbatim
/// from the existing Wii/GameCube Dolphin adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeWiiErrorKind {
    NoSelectedCheats,
    SelectionInvalid,
    UnsupportedFormat,
    Staging(GameCubeInstallPlanErrorKind),
    PreviewFailed,
    ConflictingSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeWiiError {
    pub kind: BsFreeWiiErrorKind,
    pub cheat_name: Option<String>,
    pub detail: String,
}

impl BsFreeWiiError {
    fn new(
        kind: BsFreeWiiErrorKind,
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
            kind: BsFreeWiiErrorKind::Staging(error.kind),
            cheat_name,
            detail: error.detail,
        }
    }

    fn preview(error: GameCubeInstallPlanError) -> Self {
        let cheat_name = error.cheat_name;
        Self {
            kind: BsFreeWiiErrorKind::PreviewFailed,
            cheat_name,
            detail: error.detail,
        }
    }
}

impl std::fmt::Display for BsFreeWiiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for BsFreeWiiError {}

/// Stages the selected BSFree Wii cheats for install through the existing Wii
/// GameHacking adapter (which itself reuses the shared Dolphin GameSettings
/// adapter). Deduplication is applied *before* staging, mirroring the
/// GameCube bridge exactly.
pub fn stage_bsfree_wii_install(
    staging_root: &std::path::Path,
    file_name: &str,
    destination: &DolphinIniDocument,
    destination_existed: bool,
    all_cheats: &[BsFreeWiiCheat],
    selection: &BsFreeWiiCheatSelection,
) -> Result<BsFreeStagedWiiInstall, BsFreeWiiError> {
    let selected = selection.resolve(all_cheats)?;

    let findings = analyze_bsfree_wii_duplicates(selected.iter().copied(), destination);

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
            return Err(BsFreeWiiError::new(
                BsFreeWiiErrorKind::ConflictingSelection,
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
        let already_covered_different_name = findings.iter().any(|finding| {
            finding.cheat_upstream_id == cheat.upstream_id
                && finding.kind == BsFreeWiiDedupFindingKind::AlreadyInstalledDifferentName
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
        return Err(BsFreeWiiError::new(
            BsFreeWiiErrorKind::NoSelectedCheats,
            None,
            "all selected cheats were duplicates already covered by the destination or the \
             selection",
        ));
    }

    let adapter_cheats = staged_cheats
        .iter()
        .map(|cheat| bsfree_cheat_as_wii(cheat, 0))
        .collect::<Vec<_>>();
    let selected_indices = (0..adapter_cheats.len()).collect::<Vec<_>>();

    let skipped_unselectable = all_cheats
        .iter()
        .filter(|cheat| !cheat.code_format.is_installable())
        .map(|cheat| cheat.name.clone())
        .collect::<Vec<_>>();

    let staged = stage_wii_gamehacking_install(
        staging_root,
        file_name,
        destination,
        destination_existed,
        &adapter_cheats,
        &selected_indices,
    )
    .map_err(BsFreeWiiError::staging)?;

    Ok(BsFreeStagedWiiInstall {
        staged,
        findings,
        skipped_duplicates,
        skipped_unselectable,
    })
}

/// Request for a BSFree Wii install preview. The selected archive's verified
/// Dolphin Wii Game ID is the identity the shared preview is bound to.
#[derive(Debug, Clone)]
pub struct BsFreeWiiInstallPreviewRequest {
    pub selected_archive: PathBuf,
    pub configuration_path: PathBuf,
    pub game_id: String,
    pub revision: Option<u16>,
    pub staged: StagedGameCubeIni,
}

/// The preview returned by [`build_bsfree_wii_install_preview`] - the same
/// type the existing Wii GameHacking adapter returns, since both go through
/// the identical shared preview boundary.
pub type BsFreeWiiInstallPreview = GameCubeGameHackingInstallPreview;

/// Builds the shared read-only preview for a staged BSFree Wii install, by
/// delegating to the exact preview boundary the existing Wii GameHacking
/// adapter uses. Always `VerifiedExact` on the selected archive's verified
/// Dolphin Wii Game ID; an ambiguous or missing match never reaches here.
pub fn build_bsfree_wii_install_preview(
    request: &BsFreeWiiInstallPreviewRequest,
) -> Result<GameCubeGameHackingInstallPreview, BsFreeWiiError> {
    build_dolphin_gamehacking_install_preview(
        &GameCubeGameHackingInstallPreviewRequest {
            selected_archive: request.selected_archive.clone(),
            configuration_path: request.configuration_path.clone(),
            game_id: request.game_id.clone(),
            revision: request.revision,
            staged: request.staged.clone(),
        },
        "Wii",
    )
    .map_err(BsFreeWiiError::preview)
}

/// Result of matching a BSFree Wii game against a selected archive's verified
/// identity. BSFree has no emulator-stable identifiers, so the strongest
/// achievable evidence is platform + normalized title + compatible
/// region/version - which always requires explicit user confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeWiiMatch {
    pub archive_title: String,
    pub archive_game_id: String,
    pub matched_bsfree_game_upstream_uid: i64,
    pub matched_bsfree_title: String,
    pub matched_bsfree_version: Option<String>,
    pub region_evidence: String,
    pub requires_review: bool,
    pub detail: String,
}

/// Matches a selected archive (with its verified Dolphin Wii Game ID and
/// title) to exactly one BSFree Wii game, requiring normalized title equality
/// and a compatible region where the archive can supply one. Returns `None`
/// when no candidate is strong enough - the caller then shows candidates for
/// review rather than applying.
pub fn bsfree_wii_match(
    catalogue: &BsFreeCatalogue,
    archive_title: &str,
    archive_game_id: &str,
    archive_region: Option<&str>,
) -> Result<Option<BsFreeWiiMatch>, BsFreeError> {
    let normalized_title = normalize_title(archive_title);
    let search = catalogue.search_games(&super::BsFreeGameSearchRequest {
        platform_id: Some("Wii".to_string()),
        title: archive_title.to_string(),
        version: None,
        device_id: None,
        upstream_game_id: None,
        page: super::PageRequest::games(0).bounded(),
    })?;
    let mut best: Option<BsFreeWiiMatch> = None;
    let mut tied = false;
    for game in search.page.rows {
        let game_title_normalized = normalize_title(&game.name);
        if game_title_normalized.is_empty() || game_title_normalized != normalized_title {
            continue;
        }
        let region_evidence = region_evidence(game.version.as_deref(), archive_region);
        let candidate = BsFreeWiiMatch {
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

/// How a BSFree Wii search resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeWiiSearchStatus {
    /// Exactly one BSFree Wii game matched the archive title; its classified
    /// cheats are included.
    Matched,
    /// More than one BSFree game matched; the user must confirm one.
    Candidates,
    /// No BSFree Wii game matched the search title.
    NoMatch,
}

/// Result of a BSFree Wii search for a selected archive. The archive's
/// verified Dolphin Wii Game ID is carried through for the destination
/// preview; BSFree itself contributes only platform + title + version/region
/// evidence, which always requires review before Apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BsFreeWiiSearchOutcome {
    pub status: BsFreeWiiSearchStatus,
    pub detail: String,
    pub candidates: Vec<BsFreeWiiMatch>,
    pub game: Option<BsFreeWiiMatch>,
    pub cheats: Vec<BsFreeWiiCheat>,
}

/// Searches BSFree for the selected archive's Wii game.
///
/// The shipped BSFree catalogue snapshot contains no Wii rows, so this
/// returns `NoMatch` on it; the path is implemented and tested so it
/// activates if a database containing Wii rows is loaded.
pub fn bsfree_wii_search(
    catalogue: &BsFreeCatalogue,
    archive_title: &str,
    archive_game_id: &str,
    archive_region: Option<&str>,
) -> Result<BsFreeWiiSearchOutcome, BsFreeError> {
    let base_title = strip_region_markers(archive_title);
    let probe_title = if base_title.trim().is_empty() {
        archive_title.to_string()
    } else {
        base_title
    };
    if probe_title.trim().is_empty() {
        return Ok(BsFreeWiiSearchOutcome {
            status: BsFreeWiiSearchStatus::NoMatch,
            detail: "Enter a title to search the BSFree Wii catalogue.".to_string(),
            candidates: Vec::new(),
            game: None,
            cheats: Vec::new(),
        });
    }
    let search = catalogue.search_games(&super::BsFreeGameSearchRequest {
        platform_id: Some("Wii".to_string()),
        title: probe_title.clone(),
        version: None,
        device_id: None,
        upstream_game_id: None,
        page: super::PageRequest::games(0).bounded(),
    })?;
    let mut candidates = Vec::new();
    for game in search.page.rows {
        let region_evidence = region_evidence(game.version.as_deref(), archive_region);
        candidates.push(BsFreeWiiMatch {
            archive_title: archive_title.to_string(),
            archive_game_id: archive_game_id.to_string(),
            matched_bsfree_game_upstream_uid: game.upstream_uid,
            matched_bsfree_title: game.name.clone(),
            matched_bsfree_version: game.version.clone(),
            region_evidence,
            requires_review: true,
            detail: format!(
                "BSFree Wii game {} (version {}); matched by title, requires review",
                game.name,
                game.version.as_deref().unwrap_or("not supplied")
            ),
        });
    }
    if candidates.is_empty() {
        return Ok(BsFreeWiiSearchOutcome {
            status: BsFreeWiiSearchStatus::NoMatch,
            detail: format!("No BSFree Wii game matches {:?}.", probe_title),
            candidates,
            game: None,
            cheats: Vec::new(),
        });
    }
    if candidates.len() == 1 {
        let game = candidates.remove(0);
        let cheats = bsfree_wii_cheats(catalogue, game.matched_bsfree_game_upstream_uid)?;
        return Ok(BsFreeWiiSearchOutcome {
            status: BsFreeWiiSearchStatus::Matched,
            detail: format!(
                "Matched BSFree Wii game {:?}; review the cheats before applying.",
                game.matched_bsfree_title
            ),
            candidates: Vec::new(),
            game: Some(game),
            cheats,
        });
    }
    Ok(BsFreeWiiSearchOutcome {
        status: BsFreeWiiSearchStatus::Candidates,
        detail: "Multiple BSFree Wii games match; confirm one before viewing cheats.".to_string(),
        candidates,
        game: None,
        cheats: Vec::new(),
    })
}

fn normalize_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn strip_region_markers(title: &str) -> String {
    let without_markers = title.split(['(', '[']).next().unwrap_or(title);
    without_markers.trim().to_string()
}

fn region_evidence(bsfree_version: Option<&str>, archive_region: Option<&str>) -> String {
    match (bsfree_version, archive_region) {
        (Some(version), Some(region))
            if version.to_uppercase().contains(&region.to_uppercase()) =>
        {
            format!("BSFree lists {version}; the archive's region {region} is compatible.")
        }
        (Some(version), Some(region)) => format!(
            "BSFree lists {version}; the archive's region {region} is not stated in it - review."
        ),
        (Some(version), None) => {
            format!("BSFree lists {version}; the archive's region is unknown.")
        }
        (None, Some(region)) => format!(
            "BSFree lists no version; the archive's region is {region}. Review before applying."
        ),
        (None, None) => "Neither BSFree nor the archive states a region or version.".to_string(),
    }
}

/// Loads one confirmed BSFree Wii game's cheats by its upstream UID, refusing
/// when the game is not a Wii game.
pub fn bsfree_wii_load_confirmed(
    catalogue: &BsFreeCatalogue,
    upstream_uid: i64,
) -> Result<Vec<BsFreeWiiCheat>, BsFreeError> {
    let game = catalogue
        .game(upstream_uid)?
        .ok_or_else(|| super::bsfree::BsFreeError {
            kind: super::bsfree::BsFreeErrorKind::Query,
            message: "unknown game UID".to_string(),
        })?;
    if game.system.archivefs_platform_id.as_deref() != Some("Wii") {
        return Err(super::bsfree::BsFreeError {
            kind: super::bsfree::BsFreeErrorKind::Query,
            message: format!(
                "BSFree game {:?} is not a Wii game (system {:?}); only verified Wii codes can \
                 be installed via Dolphin",
                game.name, game.system.name
            ),
        });
    }
    bsfree_wii_cheats(catalogue, upstream_uid)
}

#[cfg(test)]
mod tests;
