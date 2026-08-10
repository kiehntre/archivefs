//! Turning externally provided Gecko definitions into a real, safely-applied Dolphin install.
//!
//! ## Why the "candidate" here is unlike RetroArch's
//!
//! The provider owns discovery and inert code metadata. This adapter owns the destination and
//! transaction. An existing GameSettings file is optional destination state, never the source of
//! discoverable cheats. It may contribute already-installed state and unrelated settings that the
//! generated file must preserve.
//!
//! [`build_dolphin_candidate`] wraps the existing, unmodified
//! [`super::dolphin_local::match_dolphin_inventory`] (which already
//! implements exact game-ID and revision matching, ambiguity detection,
//! and ruling out a wrong region/revision) into the single candidate this
//! milestone needs, with its evidence made explicit for the GUI.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::dolphin_gecko_provider::{
    GeckoApplicabilityDecision, GeckoProviderEntry, GeckoProviderResult, revision_applicability,
};
use super::dolphin_local::{DolphinGameIniInventory, DolphinMatchResult, DolphinMatchState};
use super::gecko_document::{
    DolphinIniDocument, GeckoCode, merge_external_gecko_codes, parse_dolphin_ini,
    replace_gecko_enabled_section,
};
use super::shared_preview::{
    PreviewAdapter, PreviewIdentity, PreviewIdentityKind, PreviewIdentityState,
    PreviewMatchStrength, PreviewSourceItem, SharedPreviewError, SharedPreviewReport,
    SharedPreviewRequest, build_shared_preview,
};

/// Mirrors `dolphin_local::DOLPHIN_MAX_GAME_INI_BYTES`.
pub const MAX_DOLPHIN_INI_BYTES: u64 = 256 * 1024;
pub const MAX_GENERATED_INI_BYTES: usize = 512 * 1024;
pub const GENERATED_INI_PROVENANCE: &str =
    "# Gecko_Enabled section written by EmuWiz from this file's own trusted codes.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinInstallPlanErrorKind {
    /// `match_dolphin_inventory` did not return an installable state -
    /// see [`DolphinCandidateBlockedReason`] for exactly why.
    NoInstallableCandidate,
    CandidatePathUnsafe,
    CandidateMissing,
    CandidateUnreadable,
    CandidateTooLarge,
    DestinationUnsafe,
    /// Nothing was selected, or everything selected was unsafe.
    NoSelectedCodes,
    /// A selected entry does not exist in the document, or is unsafe.
    SelectionInvalid,
    StagingUnavailable,
    GeneratedFileTooLarge,
    PreviewFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinInstallPlanError {
    pub kind: DolphinInstallPlanErrorKind,
    pub path: Option<PathBuf>,
    pub detail: String,
}

impl std::fmt::Display for DolphinInstallPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for DolphinInstallPlanError {}

fn error(
    kind: DolphinInstallPlanErrorKind,
    path: Option<&Path>,
    detail: impl Into<String>,
) -> DolphinInstallPlanError {
    DolphinInstallPlanError {
        kind,
        path: path.map(Path::to_path_buf),
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------
// 1. Candidate
// ---------------------------------------------------------------------

/// Why an otherwise-found match cannot be installed. Every variant is a
/// direct, unmodified re-statement of a `DolphinMatchState` this milestone
/// does not treat as installable - never a fabricated reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinCandidateBlockedReason {
    NoVerifiedGameIdAvailable,
    InvalidVerifiedGameId,
    NoMatchingIniFound,
    /// Two or more GameSettings files match the same verified game ID -
    /// never resolved silently; the file names themselves are surfaced so
    /// the user can fix the underlying Dolphin profile if this is a
    /// mistake, since this shape does not occur in a normal Dolphin
    /// installation.
    MultipleIniFilesForGame,
    /// A file matches the game ID but not the verified revision - a
    /// revision-specific code must never be applied to a different disc
    /// revision silently.
    RevisionMismatch,
    IdentityExtractionDeferred,
}

impl DolphinCandidateBlockedReason {
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NoVerifiedGameIdAvailable => {
                "EmuWiz has no separately verified GameCube game ID for this archive yet."
            }
            Self::InvalidVerifiedGameId => {
                "The verified game ID is not three to six ASCII letters or digits."
            }
            Self::NoMatchingIniFound => {
                "No GameSettings file in this profile matches this game's verified ID."
            }
            Self::MultipleIniFilesForGame => {
                "More than one GameSettings file matches this game's verified ID. EmuWiz will \
                 not guess between them."
            }
            Self::RevisionMismatch => {
                "A GameSettings file exists for this game, but not for the verified disc \
                 revision. A revision-specific code is never applied to a different revision."
            }
            Self::IdentityExtractionDeferred => {
                "Game identity extraction for this archive's format is not available yet."
            }
        }
    }

    fn from_state(state: DolphinMatchState) -> Option<Self> {
        match state {
            DolphinMatchState::NoVerifiedGameIdAvailable => Some(Self::NoVerifiedGameIdAvailable),
            DolphinMatchState::InvalidVerifiedGameId => Some(Self::InvalidVerifiedGameId),
            DolphinMatchState::NoMatchingIniFound => Some(Self::NoMatchingIniFound),
            DolphinMatchState::MultipleIniFilesForGame => Some(Self::MultipleIniFilesForGame),
            DolphinMatchState::RevisionMismatch => Some(Self::RevisionMismatch),
            DolphinMatchState::IdentityExtractionDeferred => Some(Self::IdentityExtractionDeferred),
            DolphinMatchState::ExactGameIdMatch
            | DolphinMatchState::ExactGameIdAndRevisionMatch => None,
        }
    }
}

/// One piece of evidence behind a candidate - only ever emitted for a
/// comparison this module actually performed against data both sides
/// declared, matching the same convention `cheat_candidates` uses for
/// RetroArch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinCandidateEvidence {
    pub label: &'static str,
    pub detail: String,
}

/// The one real candidate for this archive in this profile: the matched
/// GameSettings file, or an explicit reason there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinCandidate {
    pub game_id: String,
    pub region: Option<String>,
    pub revision: Option<u16>,
    /// Absolute path of the matched GameSettings INI - both the
    /// evidence's source and, on install, the destination.
    pub path: PathBuf,
    pub cheat_count: usize,
    pub enabled_count: usize,
    pub evidence: Vec<DolphinCandidateEvidence>,
    pub installable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinCandidateOutcome {
    pub candidate: Option<DolphinCandidate>,
    pub blocked_reason: Option<DolphinCandidateBlockedReason>,
    /// Present only for `MultipleIniFilesForGame` - every conflicting
    /// path, so the user can see exactly what needs resolving.
    pub conflicting_paths: Vec<PathBuf>,
}

/// Builds the single Dolphin candidate for a verified game identity
/// against one profile's already-inspected inventory. Pure: performs no
/// filesystem access itself, reusing the caller's own already-loaded
/// `inventory` and the existing, unmodified `match_dolphin_inventory`.
#[must_use]
pub fn build_dolphin_candidate(
    inventory: &DolphinGameIniInventory,
    region: Option<&str>,
    verified_game_id: Option<&str>,
    verified_revision: Option<u16>,
) -> DolphinCandidateOutcome {
    let result: DolphinMatchResult = super::dolphin_local::match_dolphin_inventory(
        inventory,
        verified_game_id,
        verified_revision,
    );
    if let Some(reason) = DolphinCandidateBlockedReason::from_state(result.state) {
        return DolphinCandidateOutcome {
            candidate: None,
            blocked_reason: Some(reason),
            conflicting_paths: result.matching_files,
        };
    }
    let Some(path) = result.matching_files.first().cloned() else {
        return DolphinCandidateOutcome {
            candidate: None,
            blocked_reason: Some(DolphinCandidateBlockedReason::NoMatchingIniFound),
            conflicting_paths: Vec::new(),
        };
    };
    let Some(file) = inventory.files.iter().find(|file| file.path == path) else {
        return DolphinCandidateOutcome {
            candidate: None,
            blocked_reason: Some(DolphinCandidateBlockedReason::NoMatchingIniFound),
            conflicting_paths: Vec::new(),
        };
    };

    let mut evidence = vec![DolphinCandidateEvidence {
        label: "game_id",
        detail: format!(
            "verified game ID {} matches this GameSettings file's own filename exactly",
            result.verified_game_id.as_deref().unwrap_or_default()
        ),
    }];
    if let Some(revision) = result.verified_revision {
        evidence.push(DolphinCandidateEvidence {
            label: "revision",
            detail: format!(
                "verified disc revision {revision} matches (file revision: {})",
                file.revision_candidate
                    .map_or_else(|| "0 (unmarked)".to_string(), |value| value.to_string())
            ),
        });
    }
    if let Some(region) = region {
        evidence.push(DolphinCandidateEvidence {
            label: "region",
            detail: format!(
                "archive region {region} (file region byte: {})",
                file.region_candidate.as_deref().unwrap_or("unknown")
            ),
        });
    }
    evidence.push(DolphinCandidateEvidence {
        label: "source",
        detail: format!(
            "{} existing code(s), {} already enabled, in this profile's own GameSettings file",
            file.definition_count(),
            file.enabled_count()
        ),
    });

    DolphinCandidateOutcome {
        candidate: Some(DolphinCandidate {
            game_id: result.verified_game_id.unwrap_or_default(),
            region: file.region_candidate.clone(),
            revision: result.verified_revision,
            path,
            cheat_count: file.definition_count(),
            enabled_count: file.enabled_count(),
            evidence,
            installable: true,
        }),
        blocked_reason: None,
        conflicting_paths: Vec::new(),
    }
}

// ---------------------------------------------------------------------
// 2. Loading the matched file
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LoadedDolphinIni {
    pub path: PathBuf,
    pub digest: String,
    pub document: DolphinIniDocument,
}

#[derive(Debug, Clone)]
pub struct LoadedDolphinDestination {
    pub path: PathBuf,
    pub existed: bool,
    pub digest: Option<String>,
    pub document: DolphinIniDocument,
}

/// Re-reads and parses the matched GameSettings file. The path always
/// comes from an already-discovered, already-inspected
/// [`DolphinCandidate`] (itself derived from a profile the caller
/// validated as eligible), so this only re-confirms it is still a real,
/// non-symlinked, bounded-size regular file before parsing - the same
/// discipline `dolphin_local::inspect_dolphin_profile` already applies.
pub fn load_dolphin_ini(path: &Path) -> Result<LoadedDolphinIni, DolphinInstallPlanError> {
    let metadata = fs::symlink_metadata(path).map_err(|failure| {
        let kind = if failure.kind() == std::io::ErrorKind::NotFound {
            DolphinInstallPlanErrorKind::CandidateMissing
        } else {
            DolphinInstallPlanErrorKind::CandidateUnreadable
        };
        error(
            kind,
            Some(path),
            format!("GameSettings file unreadable: {failure}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(error(
            DolphinInstallPlanErrorKind::CandidatePathUnsafe,
            Some(path),
            "GameSettings file is a symlink or not a regular file; it is never followed",
        ));
    }
    if metadata.len() > MAX_DOLPHIN_INI_BYTES {
        return Err(error(
            DolphinInstallPlanErrorKind::CandidateTooLarge,
            Some(path),
            format!("GameSettings file exceeds {MAX_DOLPHIN_INI_BYTES} bytes"),
        ));
    }
    let bytes = fs::read(path).map_err(|failure| {
        error(
            DolphinInstallPlanErrorKind::CandidateUnreadable,
            Some(path),
            format!("GameSettings file could not be read: {failure}"),
        )
    })?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let document = parse_dolphin_ini(&text);
    Ok(LoadedDolphinIni {
        path: path.to_path_buf(),
        digest: hex_sha256(&bytes),
        document,
    })
}

/// Loads an optional exact-ID destination. Missing GameSettings directories and files are valid:
/// the shared transaction may create them after preview and confirmation. Existing symlinks or
/// non-directory path components are rejected before any staging occurs.
pub fn load_dolphin_destination(
    configuration_path: &Path,
    game_id: &str,
) -> Result<LoadedDolphinDestination, DolphinInstallPlanError> {
    if !configuration_path.is_absolute()
        || configuration_path.parent().is_none()
        || game_id.len() != 6
        || !game_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(error(
            DolphinInstallPlanErrorKind::DestinationUnsafe,
            Some(configuration_path),
            "Dolphin destination requires an absolute non-root profile and exact six-character game ID",
        ));
    }
    let configuration_metadata = fs::symlink_metadata(configuration_path).map_err(|failure| {
        error(
            DolphinInstallPlanErrorKind::DestinationUnsafe,
            Some(configuration_path),
            format!("Dolphin profile is inaccessible: {failure}"),
        )
    })?;
    if configuration_metadata.file_type().is_symlink() || !configuration_metadata.is_dir() {
        return Err(error(
            DolphinInstallPlanErrorKind::DestinationUnsafe,
            Some(configuration_path),
            "Dolphin profile is a symlink or not a directory",
        ));
    }
    let game_settings = configuration_path.join("GameSettings");
    match fs::symlink_metadata(&game_settings) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(error(
                DolphinInstallPlanErrorKind::DestinationUnsafe,
                Some(&game_settings),
                "Dolphin GameSettings path is a symlink or not a directory",
            ));
        }
        Ok(_) => {}
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
        Err(failure) => {
            return Err(error(
                DolphinInstallPlanErrorKind::DestinationUnsafe,
                Some(&game_settings),
                format!("Dolphin GameSettings path is inaccessible: {failure}"),
            ));
        }
    }
    let path = game_settings.join(format!("{game_id}.ini"));
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let loaded = load_dolphin_ini(&path)?;
            Ok(LoadedDolphinDestination {
                path,
                existed: true,
                digest: Some(loaded.digest),
                document: loaded.document,
            })
        }
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedDolphinDestination {
                path,
                existed: false,
                digest: None,
                document: parse_dolphin_ini(""),
            })
        }
        Err(failure) => Err(error(
            DolphinInstallPlanErrorKind::CandidateUnreadable,
            Some(&path),
            format!("Dolphin destination is inaccessible: {failure}"),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinProviderCodeSelectionEntry {
    pub index: usize,
    pub provider_entry_id: String,
    pub name: String,
    pub selectable: bool,
    pub selected: bool,
    pub already_present: bool,
    pub already_enabled: bool,
    pub uncertain_revision: bool,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinProviderCodeSelection {
    pub entries: Vec<DolphinProviderCodeSelectionEntry>,
}

impl DolphinProviderCodeSelection {
    #[must_use]
    pub fn from_provider(
        provider: &GeckoProviderResult,
        destination: &LoadedDolphinDestination,
    ) -> Self {
        let entries = provider
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let existing = destination
                    .document
                    .gecko_codes
                    .iter()
                    .find(|code| code.name == entry.name);
                let already_present = existing
                    .is_some_and(|code| code.lines == entry.code_lines);
                let body_conflict = existing.is_some() && !already_present;
                let applicability =
                    revision_applicability(entry.revision_applicability, provider.revision);
                let mut warnings = entry.parse_warnings.clone();
                if body_conflict {
                    warnings.push(
                        "An existing code has this name but a different body; it will not be overwritten."
                            .to_string(),
                    );
                }
                if applicability == GeckoApplicabilityDecision::Reject {
                    warnings.push(format!(
                        "This code does not apply to disc revision {}.",
                        provider.revision
                    ));
                }
                let already_enabled = destination
                    .document
                    .gecko_enabled_names
                    .iter()
                    .any(|name| name == &entry.name);
                DolphinProviderCodeSelectionEntry {
                    index,
                    provider_entry_id: entry.provider_entry_id.clone(),
                    name: entry.name.clone(),
                    selectable: entry.safe_to_offer
                        && !body_conflict
                        && applicability != GeckoApplicabilityDecision::Reject,
                    selected: already_enabled && already_present,
                    already_present,
                    already_enabled,
                    uncertain_revision: applicability
                        == GeckoApplicabilityDecision::OfferWithWarning,
                    notes: entry.notes.clone(),
                    warnings,
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
    pub fn can_preview(&self) -> bool {
        self.selected_count() > 0
    }

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

    pub fn resolve_names(
        &self,
        provider: &GeckoProviderResult,
    ) -> Result<Vec<String>, DolphinInstallPlanError> {
        let mut names = Vec::new();
        for selection in self.entries.iter().filter(|entry| entry.selected) {
            let entry = provider.entries.get(selection.index).ok_or_else(|| {
                error(
                    DolphinInstallPlanErrorKind::SelectionInvalid,
                    None,
                    "selected provider entry no longer exists",
                )
            })?;
            if entry.provider_entry_id != selection.provider_entry_id
                || !selection.selectable
                || !entry.safe_to_offer
            {
                return Err(error(
                    DolphinInstallPlanErrorKind::SelectionInvalid,
                    None,
                    format!(
                        "selected provider code {:?} is not safe to install",
                        entry.name
                    ),
                ));
            }
            names.push(entry.name.clone());
        }
        if names.is_empty() {
            return Err(error(
                DolphinInstallPlanErrorKind::NoSelectedCodes,
                None,
                "no provider codes are selected; choose at least one before preview",
            ));
        }
        Ok(names)
    }
}

// ---------------------------------------------------------------------
// 3. Code selection
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinCodeSelectionEntry {
    /// Position in `document.gecko_codes` - stable across a selection
    /// session, and the key the GUI uses when toggling.
    pub index: usize,
    pub name: String,
    pub selectable: bool,
    pub selected: bool,
    /// Whether this code is already listed in the file's own
    /// `[Gecko_Enabled]` section - "installed" is a fact about the file
    /// found on disk; "selected" is only this session's in-progress
    /// choice.
    pub already_enabled: bool,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    pub has_blocking_warning: bool,
}

/// The whole picker state for the matched file's own codes. Construction
/// preserves the file's existing enabled set as the starting selection -
/// unlike RetroArch's "nothing selected by default" (a brand new install),
/// this file may already have codes the user turned on previously, and
/// losing that state on merely opening the picker would be a surprising,
/// silent change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinCodeSelection {
    pub entries: Vec<DolphinCodeSelectionEntry>,
}

impl DolphinCodeSelection {
    #[must_use]
    pub fn from_document(document: &DolphinIniDocument) -> Self {
        Self {
            entries: document
                .gecko_codes
                .iter()
                .enumerate()
                .map(|(index, code)| entry_row(index, code, &document.gecko_enabled_names))
                .collect(),
        }
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

    /// Ticks one entry. Returns false - changing nothing - for an unknown
    /// or unsafe entry, so an unsafe code can never become selected
    /// through any path.
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

    /// The selected codes' names, in catalogue order - exactly what
    /// `replace_gecko_enabled_section` needs. Returns an error rather than
    /// silently dropping a selection that no longer resolves against
    /// `document`.
    pub fn resolve_names(
        &self,
        document: &DolphinIniDocument,
    ) -> Result<Vec<String>, DolphinInstallPlanError> {
        let mut names = Vec::new();
        for row in self.entries.iter().filter(|entry| entry.selected) {
            let code = document.gecko_codes.get(row.index).ok_or_else(|| {
                error(
                    DolphinInstallPlanErrorKind::SelectionInvalid,
                    None,
                    format!("selected code {} is not in the matched file", row.index),
                )
            })?;
            if !code.is_selectable() {
                return Err(error(
                    DolphinInstallPlanErrorKind::SelectionInvalid,
                    None,
                    format!("selected code {:?} is not safe to install", code.name),
                ));
            }
            names.push(code.name.clone());
        }
        if names.is_empty() {
            return Err(error(
                DolphinInstallPlanErrorKind::NoSelectedCodes,
                None,
                "no codes are selected; choose at least one before applying",
            ));
        }
        Ok(names)
    }
}

fn entry_row(
    index: usize,
    code: &GeckoCode,
    enabled_names: &[String],
) -> DolphinCodeSelectionEntry {
    let already_enabled = enabled_names.iter().any(|name| name == &code.name);
    DolphinCodeSelectionEntry {
        index,
        name: code.name.clone(),
        selectable: code.is_selectable(),
        selected: already_enabled && code.is_selectable(),
        already_enabled,
        notes: code.notes.clone(),
        warnings: code
            .warnings
            .iter()
            .map(|warning| warning.detail.clone())
            .collect(),
        has_blocking_warning: !code.warnings.is_empty(),
    }
}

// ---------------------------------------------------------------------
// 4. Staging and preview
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StagedDolphinIni {
    pub staging_root: PathBuf,
    pub path: PathBuf,
    pub digest: String,
    pub contents: String,
    pub selected_code_count: usize,
    pub selected_code_names: Vec<String>,
    pub destination_existed: bool,
    pub preserved_sections: Vec<String>,
}

/// Renders the full file with only `[Gecko_Enabled]` replaced, and writes
/// it atomically into a private staging directory - the same reason
/// RetroArch's `stage_generated_cheat_file` exists: the transaction
/// machinery installs files by digest from an approved source root, and a
/// generated/edited body has no separate file in the profile to point at.
/// The real GameSettings file is never written to directly here.
pub fn stage_dolphin_ini(
    staging_root: &Path,
    file_name: &str,
    document: &DolphinIniDocument,
    selected_names: &[String],
) -> Result<StagedDolphinIni, DolphinInstallPlanError> {
    if selected_names.is_empty() {
        return Err(error(
            DolphinInstallPlanErrorKind::NoSelectedCodes,
            None,
            "refusing to stage a GameSettings file with no codes selected",
        ));
    }
    let contents = replace_gecko_enabled_section(document, selected_names);
    if contents.len() > MAX_GENERATED_INI_BYTES {
        return Err(error(
            DolphinInstallPlanErrorKind::GeneratedFileTooLarge,
            None,
            format!("generated GameSettings file exceeds {MAX_GENERATED_INI_BYTES} bytes"),
        ));
    }
    fs::create_dir_all(staging_root).map_err(|failure| {
        error(
            DolphinInstallPlanErrorKind::StagingUnavailable,
            Some(staging_root),
            format!("staging directory unavailable: {failure}"),
        )
    })?;
    let path = staging_root.join(file_name);
    let temporary = staging_root.join(format!(".{file_name}.partial"));
    fs::write(&temporary, &contents).map_err(|failure| {
        error(
            DolphinInstallPlanErrorKind::StagingUnavailable,
            Some(&temporary),
            format!("staged file could not be written: {failure}"),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|failure| {
        let _ = fs::remove_file(&temporary);
        error(
            DolphinInstallPlanErrorKind::StagingUnavailable,
            Some(&path),
            format!("staged file could not be finalized: {failure}"),
        )
    })?;
    Ok(StagedDolphinIni {
        staging_root: staging_root.to_path_buf(),
        path,
        digest: hex_sha256(contents.as_bytes()),
        contents,
        selected_code_count: selected_names.len(),
        selected_code_names: selected_names.to_vec(),
        destination_existed: true,
        preserved_sections: document.section_names(),
    })
}

/// Stages a generated destination from external provider definitions. The provider result is inert
/// input; only this adapter knows the existing destination or Dolphin file format.
pub fn stage_dolphin_provider_ini(
    staging_root: &Path,
    destination: &LoadedDolphinDestination,
    provider: &GeckoProviderResult,
    selection: &DolphinProviderCodeSelection,
) -> Result<StagedDolphinIni, DolphinInstallPlanError> {
    let expected_file_name = format!("{}.ini", provider.game_id);
    if provider.game_id.len() != 6
        || destination.path.file_name().and_then(|name| name.to_str())
            != Some(expected_file_name.as_str())
    {
        return Err(error(
            DolphinInstallPlanErrorKind::SelectionInvalid,
            Some(&destination.path),
            "provider game ID does not match the exact Dolphin destination",
        ));
    }
    let selected_names = selection.resolve_names(provider)?;
    let provider_codes: Vec<GeckoCode> = provider
        .entries
        .iter()
        .filter(|entry| entry.safe_to_offer)
        .map(provider_entry_as_gecko_code)
        .collect();
    let contents =
        merge_external_gecko_codes(&destination.document, &provider_codes, &selected_names)
            .map_err(|failure| {
                error(
                    DolphinInstallPlanErrorKind::SelectionInvalid,
                    Some(&destination.path),
                    failure.to_string(),
                )
            })?;
    if contents.len() > MAX_GENERATED_INI_BYTES {
        return Err(error(
            DolphinInstallPlanErrorKind::GeneratedFileTooLarge,
            Some(&destination.path),
            format!("generated GameSettings file exceeds {MAX_GENERATED_INI_BYTES} bytes"),
        ));
    }
    fs::create_dir_all(staging_root).map_err(|failure| {
        error(
            DolphinInstallPlanErrorKind::StagingUnavailable,
            Some(staging_root),
            format!("staging directory unavailable: {failure}"),
        )
    })?;
    let file_name = destination
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            error(
                DolphinInstallPlanErrorKind::DestinationUnsafe,
                Some(&destination.path),
                "Dolphin destination has no usable filename",
            )
        })?;
    let path = staging_root.join(file_name);
    let temporary = staging_root.join(format!(".{file_name}.partial"));
    fs::write(&temporary, &contents).map_err(|failure| {
        error(
            DolphinInstallPlanErrorKind::StagingUnavailable,
            Some(&temporary),
            format!("staged file could not be written: {failure}"),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|failure| {
        let _ = fs::remove_file(&temporary);
        error(
            DolphinInstallPlanErrorKind::StagingUnavailable,
            Some(&path),
            format!("staged file could not be finalized: {failure}"),
        )
    })?;
    Ok(StagedDolphinIni {
        staging_root: staging_root.to_path_buf(),
        path,
        digest: hex_sha256(contents.as_bytes()),
        contents,
        selected_code_count: selected_names.len(),
        selected_code_names: selected_names,
        destination_existed: destination.existed,
        preserved_sections: destination.document.section_names(),
    })
}

fn provider_entry_as_gecko_code(entry: &GeckoProviderEntry) -> GeckoCode {
    GeckoCode {
        name: entry.name.clone(),
        lines: entry.code_lines.clone(),
        notes: entry.notes.clone(),
        enabled_by_default: false,
        warnings: Vec::new(),
    }
}

#[derive(Debug, Clone)]
pub struct DolphinInstallPreviewRequest {
    pub selected_archive: PathBuf,
    /// The Dolphin profile's own configuration root - the destination
    /// root every preview entry is relative to (matches the real
    /// `<configuration_path>/GameSettings/<GameID>.ini` layout exactly).
    pub configuration_path: PathBuf,
    pub game_id: String,
    pub revision: Option<u16>,
    pub staged: StagedDolphinIni,
}

#[derive(Debug, Clone)]
pub struct DolphinInstallPreview {
    pub report: SharedPreviewReport,
    pub staged: StagedDolphinIni,
}

/// Wraps the staged file in the same shared preview every write-capable
/// adapter uses. Always `VerifiedExact` - by the time this is called, a
/// `DolphinCandidate` already required an exact game-ID and revision
/// match; `shared_preview` itself additionally requires `VerifiedExact`
/// specifically for the Dolphin adapter (never merely `Strong`), so there
/// is no weaker tier to map an ambiguous or missing match onto - those
/// never reach this function at all.
pub fn build_dolphin_install_preview(
    request: &DolphinInstallPreviewRequest,
) -> Result<DolphinInstallPreview, DolphinInstallPlanError> {
    let file_name = request
        .staged
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            error(
                DolphinInstallPlanErrorKind::StagingUnavailable,
                Some(&request.staged.path),
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
        platform: Some("GameCube".to_string()),
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
    .map_err(|failure| preview_error(&failure))?;
    Ok(DolphinInstallPreview {
        report,
        staged: request.staged.clone(),
    })
}

fn preview_error(failure: &SharedPreviewError) -> DolphinInstallPlanError {
    DolphinInstallPlanError {
        kind: DolphinInstallPlanErrorKind::PreviewFailed,
        path: None,
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
