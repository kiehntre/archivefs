//! Turning classified GameHacking.org GameCube cheats into a real, safely
//! applied Dolphin GameSettings install.
//!
//! ## Scope: only `ActionReplay` and `Gecko` are ever installable
//!
//! `GameCubeCodeFormat::RawUnknown` and `::Unsupported` cheats can never be
//! selected, merged, staged, or previewed by anything in this module -
//! `GameCubeCheatSelection::from_cheats` marks them unselectable at
//! construction, and every merge/removal entry point re-checks the format
//! again before touching a file, so no caller bug can smuggle one through.
//! EmuWiz never guesses which Dolphin section an unlabeled or malformed
//! code belongs in (see `GameCubeCodeFormat`'s own doc comment for why that
//! would be an unsafe guess), so those stay preview-only forever, not
//! merely "not yet supported".
//!
//! ## Why a separate EmuWiz-managed tracking section, not inline markers
//!
//! Dolphin's `[Gecko]`/`[ActionReplay]` bodies are parsed line-by-line by
//! `gecko_document`'s existing `parse_gecko_codes`: every line between two
//! `$Name` headers is attributed to the current code as a hex line, a
//! `*Note`, or a malformed-line warning. An inline `// EmuWiz managed`
//! comment inside a code's body would therefore be misparsed as a bogus
//! code line on every future read. Recording which code *names* EmuWiz
//! itself installed in a wholly separate, inert section
//! ([`MANAGED_SECTION_NAME`]) avoids that entirely: Dolphin ignores an
//! unknown section, and `gecko_document` preserves it byte-for-byte like
//! any other section it doesn't specifically understand.
//!
//! ## Idempotency and conflicts
//!
//! Re-installing the same selection twice is a no-op: `gecko_document`'s
//! merge functions already treat a same-name, same-body existing code as
//! nothing to do. A same-name code with a *different* body - whether or
//! not it happens to be EmuWiz-managed - is always a hard error; this
//! milestone never silently overwrites a code's content, only adds new
//! ones or removes exactly the names it itself tracks as managed.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::gamehacking_gamecube_provider::{GameCubeCodeFormat, GameHackingGameCubeCheat};
use super::gecko_document::{
    DolphinCodeSectionKind, DolphinIniDocument, GeckoCode, is_gecko_code_line,
    merge_external_action_replay_codes, merge_external_gecko_codes, parse_dolphin_ini,
    remove_named_codes, replace_named_section,
};
use super::shared_preview::{
    PreviewAdapter, PreviewIdentity, PreviewIdentityKind, PreviewIdentityState,
    PreviewMatchStrength, PreviewSourceItem, SharedPreviewError, SharedPreviewReport,
    SharedPreviewRequest, build_shared_preview,
};

pub const MAX_GENERATED_INI_BYTES: usize = 512 * 1024;

/// The bookkeeping section name EmuWiz writes into a Dolphin
/// GameSettings file - see the module doc comment for why this exists
/// instead of inline markers. Body shape: one `$Name` line per managed
/// code, exactly like `[Gecko_Enabled]`'s own body.
pub const MANAGED_SECTION_NAME: &str = "ArchiveFS_Managed_GameHacking";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameCubeInstallPlanErrorKind {
    NoSelectedCheats,
    SelectionInvalid,
    UnsupportedFormat,
    NotManaged,
    GeneratedFileTooLarge,
    StagingUnavailable,
    PreviewFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameCubeInstallPlanError {
    pub kind: GameCubeInstallPlanErrorKind,
    pub cheat_name: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for GameCubeInstallPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GameCubeInstallPlanError {}

fn error(
    kind: GameCubeInstallPlanErrorKind,
    cheat_name: Option<&str>,
    detail: impl Into<String>,
) -> GameCubeInstallPlanError {
    GameCubeInstallPlanError {
        kind,
        cheat_name: cheat_name.map(str::to_string),
        detail: detail.into(),
    }
}

fn map_merge_error(failure: super::gecko_document::GeckoMergeError) -> GameCubeInstallPlanError {
    error(
        GameCubeInstallPlanErrorKind::SelectionInvalid,
        failure.code_name.as_deref(),
        failure.detail,
    )
}

/// Dolphin's own convention for a code's display name is `"Display Name
/// [Author]"` (see `GeckoCode::name`'s doc comment) - never split apart by
/// Dolphin itself, and reproduced exactly here so a cheat installed by
/// EmuWiz looks identical to one Dolphin's own catalogue would offer.
#[must_use]
pub fn dolphin_code_name(cheat: &GameHackingGameCubeCheat) -> String {
    match cheat.author.as_deref().map(str::trim) {
        Some(author) if !author.is_empty() => format!("{} [{author}]", cheat.name),
        _ => format!("{} [GameHacking.org]", cheat.name),
    }
}

/// Exactly the set of code names this file's `[MANAGED_SECTION_NAME]`
/// section lists - the only names removal is ever allowed to touch.
#[must_use]
pub fn managed_names(document: &DolphinIniDocument) -> std::collections::BTreeSet<String> {
    document
        .named_section_lines(MANAGED_SECTION_NAME)
        .iter()
        .filter_map(|raw| raw.trim().strip_prefix('$'))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

// ---------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameCubeCheatSelectionEntry {
    /// Position in the caller's own fetched cheat list - stable across a
    /// selection session, and the key the GUI uses when toggling.
    pub index: usize,
    pub id: String,
    pub name: String,
    pub code_format: GameCubeCodeFormat,
    /// `true` only for `ActionReplay`/`Gecko` cheats with at least one
    /// well-formed `XXXXXXXX YYYYYYYY` code line. `RawUnknown` and
    /// `Unsupported` cheats are always `false` here and can never become
    /// `true` through any call on this type.
    pub selectable: bool,
    pub selected: bool,
    pub already_managed: bool,
    /// The exact Dolphin section name this cheat would be (or already is)
    /// installed under - `dolphin_code_name(cheat)`. Precomputed so a
    /// caller doing removal never has to reconstruct it (and risk
    /// diverging from the install path) to match against
    /// [`managed_names`].
    pub dolphin_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameCubeCheatSelection {
    pub entries: Vec<GameCubeCheatSelectionEntry>,
}

impl GameCubeCheatSelection {
    #[must_use]
    pub fn from_cheats(
        cheats: &[GameHackingGameCubeCheat],
        destination: &DolphinIniDocument,
    ) -> Self {
        let managed = managed_names(destination);
        let entries = cheats
            .iter()
            .enumerate()
            .map(|(index, cheat)| {
                let selectable = matches!(
                    cheat.code_format,
                    GameCubeCodeFormat::ActionReplay | GameCubeCodeFormat::Gecko
                ) && !cheat.code_lines.is_empty()
                    && cheat.code_lines.iter().all(|line| is_gecko_code_line(line));
                let dolphin_name = dolphin_code_name(cheat);
                GameCubeCheatSelectionEntry {
                    index,
                    id: cheat.id.clone(),
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

    /// Ticks one entry. Returns `false` - changing nothing - for an
    /// unknown or unselectable entry, so a `RawUnknown`/`Unsupported`
    /// cheat can never become selected through any path.
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

    /// The selected cheats, re-validated against the caller's own fetched
    /// list and re-checked for format eligibility - never trusts a stale
    /// `index`/`selectable` flag alone.
    pub fn resolve<'a>(
        &self,
        cheats: &'a [GameHackingGameCubeCheat],
    ) -> Result<Vec<&'a GameHackingGameCubeCheat>, GameCubeInstallPlanError> {
        let mut selected = Vec::new();
        for row in self.entries.iter().filter(|entry| entry.selected) {
            let cheat = cheats.get(row.index).ok_or_else(|| {
                error(
                    GameCubeInstallPlanErrorKind::SelectionInvalid,
                    Some(row.name.as_str()),
                    "selected cheat is no longer in the fetched list",
                )
            })?;
            if cheat.id != row.id || !row.selectable {
                return Err(error(
                    GameCubeInstallPlanErrorKind::SelectionInvalid,
                    Some(cheat.name.as_str()),
                    "selected cheat is not safe to install",
                ));
            }
            if !matches!(
                cheat.code_format,
                GameCubeCodeFormat::ActionReplay | GameCubeCodeFormat::Gecko
            ) {
                return Err(error(
                    GameCubeInstallPlanErrorKind::UnsupportedFormat,
                    Some(cheat.name.as_str()),
                    format!(
                        "{:?} cheats are preview-only and can never be installed",
                        cheat.code_format
                    ),
                ));
            }
            selected.push(cheat);
        }
        if selected.is_empty() {
            return Err(error(
                GameCubeInstallPlanErrorKind::NoSelectedCheats,
                None,
                "no ActionReplay or Gecko cheats are selected; choose at least one before installing",
            ));
        }
        Ok(selected)
    }
}

// ---------------------------------------------------------------------
// Staging: install
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedGameCubeCheat {
    pub name: String,
    pub dolphin_name: String,
    pub code_format: GameCubeCodeFormat,
}

#[derive(Debug, Clone)]
pub struct StagedGameCubeIni {
    pub staging_root: PathBuf,
    pub path: PathBuf,
    pub digest: String,
    pub contents: String,
    pub destination_existed: bool,
    /// Every cheat this call installed or removed, with the exact Dolphin
    /// section it was routed to - the per-cheat detail requirement 11's
    /// confirmation preview needs.
    pub affected: Vec<StagedGameCubeCheat>,
    /// Every `RawUnknown`/`Unsupported` cheat in the caller's full fetched
    /// list, skipped regardless of selection - shown so the confirmation
    /// preview can list them explicitly.
    pub skipped_unselectable: Vec<String>,
}

fn write_staged(
    staging_root: &Path,
    file_name: &str,
    contents: &str,
) -> Result<(PathBuf, String), GameCubeInstallPlanError> {
    if contents.len() > MAX_GENERATED_INI_BYTES {
        return Err(error(
            GameCubeInstallPlanErrorKind::GeneratedFileTooLarge,
            None,
            format!("generated GameSettings file exceeds {MAX_GENERATED_INI_BYTES} bytes"),
        ));
    }
    fs::create_dir_all(staging_root).map_err(|failure| {
        error(
            GameCubeInstallPlanErrorKind::StagingUnavailable,
            None,
            format!("staging directory unavailable: {failure}"),
        )
    })?;
    let path = staging_root.join(file_name);
    let temporary = staging_root.join(format!(".{file_name}.partial"));
    fs::write(&temporary, contents).map_err(|failure| {
        error(
            GameCubeInstallPlanErrorKind::StagingUnavailable,
            None,
            format!("staged file could not be written: {failure}"),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|failure| {
        let _ = fs::remove_file(&temporary);
        error(
            GameCubeInstallPlanErrorKind::StagingUnavailable,
            None,
            format!("staged file could not be finalized: {failure}"),
        )
    })?;
    Ok((path, hex_sha256(contents.as_bytes())))
}

fn skipped_unselectable_names(cheats: &[GameHackingGameCubeCheat]) -> Vec<String> {
    cheats
        .iter()
        .filter(|cheat| {
            matches!(
                cheat.code_format,
                GameCubeCodeFormat::RawUnknown | GameCubeCodeFormat::Unsupported
            )
        })
        .map(|cheat| cheat.name.clone())
        .collect()
}

/// Stages the selected cheats' install: routes each to `[Gecko]` or
/// `[ActionReplay]` per its own classification (never converted between
/// the two - see requirement 14), preserves every unrelated section
/// byte-for-byte, and records the newly installed names in
/// [`MANAGED_SECTION_NAME`]. Writes only into `staging_root`; the real
/// destination is written later by the shared apply pipeline.
pub fn stage_gamecube_gamehacking_install(
    staging_root: &Path,
    file_name: &str,
    destination: &DolphinIniDocument,
    destination_existed: bool,
    all_cheats: &[GameHackingGameCubeCheat],
    selection: &GameCubeCheatSelection,
) -> Result<StagedGameCubeIni, GameCubeInstallPlanError> {
    let selected = selection.resolve(all_cheats)?;

    let mut gecko_codes: Vec<GeckoCode> = Vec::new();
    let mut gecko_names: Vec<String> = Vec::new();
    let mut ar_codes: Vec<GeckoCode> = Vec::new();
    let mut ar_names: Vec<String> = Vec::new();
    let mut affected: Vec<StagedGameCubeCheat> = Vec::new();

    for cheat in &selected {
        let dolphin_name = dolphin_code_name(cheat);
        let code = GeckoCode {
            name: dolphin_name.clone(),
            lines: cheat.code_lines.clone(),
            notes: Vec::new(),
            enabled_by_default: false,
            warnings: Vec::new(),
        };
        match cheat.code_format {
            GameCubeCodeFormat::Gecko => {
                gecko_names.push(dolphin_name.clone());
                gecko_codes.push(code);
            }
            GameCubeCodeFormat::ActionReplay => {
                ar_names.push(dolphin_name.clone());
                ar_codes.push(code);
            }
            GameCubeCodeFormat::RawUnknown | GameCubeCodeFormat::Unsupported => unreachable!(
                "GameCubeCheatSelection::resolve only ever returns ActionReplay/Gecko cheats"
            ),
        }
        affected.push(StagedGameCubeCheat {
            name: cheat.name.clone(),
            dolphin_name,
            code_format: cheat.code_format,
        });
    }

    let mut document = destination.clone();
    if !gecko_codes.is_empty() {
        let rendered = merge_external_gecko_codes(&document, &gecko_codes, &gecko_names)
            .map_err(map_merge_error)?;
        document = parse_dolphin_ini(&rendered);
    }
    if !ar_codes.is_empty() {
        let rendered = merge_external_action_replay_codes(&document, &ar_codes, &ar_names)
            .map_err(map_merge_error)?;
        document = parse_dolphin_ini(&rendered);
    }

    let mut managed = managed_names(&document);
    for name in gecko_names.iter().chain(ar_names.iter()) {
        managed.insert(name.clone());
    }
    let managed_lines: Vec<String> = managed.iter().map(|name| format!("${name}")).collect();
    let contents = replace_named_section(&document, MANAGED_SECTION_NAME, managed_lines);

    let (path, digest) = write_staged(staging_root, file_name, &contents)?;
    Ok(StagedGameCubeIni {
        staging_root: staging_root.to_path_buf(),
        path,
        digest,
        contents,
        destination_existed,
        affected,
        skipped_unselectable: skipped_unselectable_names(all_cheats),
    })
}

// ---------------------------------------------------------------------
// Staging: removal
// ---------------------------------------------------------------------

/// Stages removal of exactly the given EmuWiz-managed code names.
/// Refuses to touch any name not currently listed in
/// [`MANAGED_SECTION_NAME`] - including a same-named code the user added
/// themselves - so removal can never delete a code EmuWiz did not
/// itself install.
pub fn stage_gamecube_gamehacking_removal(
    staging_root: &Path,
    file_name: &str,
    destination: &DolphinIniDocument,
    destination_existed: bool,
    remove_dolphin_names: &[String],
) -> Result<StagedGameCubeIni, GameCubeInstallPlanError> {
    let managed = managed_names(destination);
    let mut to_remove = Vec::new();
    for name in remove_dolphin_names {
        if !managed.contains(name) {
            return Err(error(
                GameCubeInstallPlanErrorKind::NotManaged,
                Some(name.as_str()),
                format!(
                    "{name:?} is not an EmuWiz-managed GameHacking code; EmuWiz will not \
                     remove a code it did not install"
                ),
            ));
        }
        to_remove.push(name.clone());
    }
    if to_remove.is_empty() {
        return Err(error(
            GameCubeInstallPlanErrorKind::NoSelectedCheats,
            None,
            "no EmuWiz-managed codes selected for removal",
        ));
    }

    let gecko_targets: Vec<String> = to_remove
        .iter()
        .filter(|name| {
            destination
                .gecko_codes
                .iter()
                .any(|code| &code.name == *name)
        })
        .cloned()
        .collect();
    let ar_targets: Vec<String> = to_remove
        .iter()
        .filter(|name| {
            destination
                .action_replay_codes
                .iter()
                .any(|code| &code.name == *name)
        })
        .cloned()
        .collect();

    let mut document = destination.clone();
    if !gecko_targets.is_empty() {
        let rendered = remove_named_codes(&document, DolphinCodeSectionKind::Gecko, &gecko_targets);
        document = parse_dolphin_ini(&rendered);
    }
    if !ar_targets.is_empty() {
        let rendered =
            remove_named_codes(&document, DolphinCodeSectionKind::ActionReplay, &ar_targets);
        document = parse_dolphin_ini(&rendered);
    }

    let mut managed_after = managed_names(&document);
    for name in &to_remove {
        managed_after.remove(name);
    }
    let managed_lines: Vec<String> = managed_after
        .iter()
        .map(|name| format!("${name}"))
        .collect();
    let contents = replace_named_section(&document, MANAGED_SECTION_NAME, managed_lines);

    let affected = to_remove
        .iter()
        .map(|name| StagedGameCubeCheat {
            name: name.clone(),
            dolphin_name: name.clone(),
            code_format: if gecko_targets.contains(name) {
                GameCubeCodeFormat::Gecko
            } else {
                GameCubeCodeFormat::ActionReplay
            },
        })
        .collect();

    let (path, digest) = write_staged(staging_root, file_name, &contents)?;
    Ok(StagedGameCubeIni {
        staging_root: staging_root.to_path_buf(),
        path,
        digest,
        contents,
        destination_existed,
        affected,
        skipped_unselectable: Vec::new(),
    })
}

// ---------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GameCubeGameHackingInstallPreviewRequest {
    pub selected_archive: PathBuf,
    /// The Dolphin profile's own configuration root - matches the real
    /// `<configuration_path>/GameSettings/<GameID>.ini` layout exactly.
    pub configuration_path: PathBuf,
    pub game_id: String,
    pub revision: Option<u16>,
    pub staged: StagedGameCubeIni,
}

#[derive(Debug, Clone)]
pub struct GameCubeGameHackingInstallPreview {
    pub report: SharedPreviewReport,
    pub staged: StagedGameCubeIni,
}

/// Wraps the staged file in the same shared preview every write-capable
/// adapter uses. Always `VerifiedExact`, matching `build_dolphin_install_preview`'s
/// own Dolphin-specific requirement - there is no weaker tier this ever
/// maps an ambiguous or missing match onto; those never reach here.
pub fn build_gamecube_gamehacking_install_preview(
    request: &GameCubeGameHackingInstallPreviewRequest,
) -> Result<GameCubeGameHackingInstallPreview, GameCubeInstallPlanError> {
    build_dolphin_gamehacking_install_preview(request, "GameCube")
}

/// Shared Dolphin GameSettings preview boundary for GameCube and Wii.
/// The public GameCube wrapper remains unchanged; the Wii proof adapter
/// supplies only its already-verified platform label.
pub(crate) fn build_dolphin_gamehacking_install_preview(
    request: &GameCubeGameHackingInstallPreviewRequest,
    platform: &str,
) -> Result<GameCubeGameHackingInstallPreview, GameCubeInstallPlanError> {
    let file_name = request
        .staged
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            error(
                GameCubeInstallPlanErrorKind::StagingUnavailable,
                None,
                "staged file has no usable filename",
            )
        })?;
    let relative = PathBuf::from("GameSettings").join(file_name);
    let identity_value = match request.revision {
        Some(revision) => format!("{}:r{revision}", request.game_id),
        None => request.game_id.clone(),
    };
    let report = build_shared_preview(&SharedPreviewRequest {
        adapter: PreviewAdapter::Dolphin,
        selected_archive: request.selected_archive.clone(),
        platform: Some(platform.to_string()),
        identity: PreviewIdentity {
            kind: PreviewIdentityKind::DolphinGameId,
            state: PreviewIdentityState::Verified,
            value: Some(identity_value),
            archive_path: request.selected_archive.clone(),
            revision: request.revision,
        },
        destination_root: request.configuration_path.clone(),
        source_items: vec![PreviewSourceItem {
            adapter: PreviewAdapter::Dolphin,
            source_path: request.staged.path.clone(),
            expected_source_digest: Some(request.staged.digest.clone()),
            destination_relative_paths: vec![relative],
            match_strength: PreviewMatchStrength::VerifiedExact,
        }],
    })
    .map_err(preview_error)?;
    Ok(GameCubeGameHackingInstallPreview {
        report,
        staged: request.staged.clone(),
    })
}

fn preview_error(failure: SharedPreviewError) -> GameCubeInstallPlanError {
    GameCubeInstallPlanError {
        kind: GameCubeInstallPlanErrorKind::PreviewFailed,
        cheat_name: None,
        detail: failure.to_string(),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
