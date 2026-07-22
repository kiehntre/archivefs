//! Reusable orchestration for guided local RetroArch cheat setup.
//!
//! This module discovers and classifies profiles, resolves an exact profile
//! selection, and composes the existing read-only catalogue/matching preview.
//! It deliberately contains no installer implementation: callers apply
//! [`RetroArchCheatSetupPlan::installer_entries`] with
//! [`super::execute_cheat_install_run`], which remains the sole write path.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::emulator_environment::retroarch::{
    AppImageIdentificationConfidence, ConfigAssociation, ConfigReadOutcome, Diagnostic,
    DiscoveryEnvironment, ExecutableState, PathPurpose, ProfileKind, ProfileScope, ResolutionState,
    RetroArchEnvironmentReport, RetroArchProfile, discover_retroarch_environment,
};
use crate::emulator_environment::{EncodedPath, FsProbe, ReadOnlyHostFilesystem};

use super::retroarch::{
    build_retroarch_advisory_plan, load_retroarch_catalogue_archives_read_only,
};
use super::{
    CheatAvailabilityEntry, CheatInstallOutcome, CheatInstallPath, CheatInstallRun,
    CheatMatchConfidence, CheatStagingAction, PatchManagerError, build_cheat_availability_report,
    load_catalogue_evidence_read_only, load_cheat_catalogue_snapshot, plan_cheat_install_entries,
    validate_destination_root,
};

pub const RETROARCH_CHEAT_SETUP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroArchCheatSetupProfileState {
    Eligible,
    Ineligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetroArchCheatSetupProfileBlocker {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchCheatSetupProfile {
    pub profile_id: String,
    pub installation_type: ProfileKind,
    pub scope: ProfileScope,
    pub state: RetroArchCheatSetupProfileState,
    pub eligible: bool,
    pub executable_evidence: Vec<EncodedPath>,
    pub configuration_path: EncodedPath,
    pub cheat_destination_root: Option<EncodedPath>,
    pub blockers: Vec<RetroArchCheatSetupProfileBlocker>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchCheatSetupDiscovery {
    pub schema_version: u32,
    pub profiles: Vec<RetroArchCheatSetupProfile>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing)]
    pub environment: RetroArchEnvironmentReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetroArchCheatSetupMessage {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetroArchCheatSetupError {
    pub code: String,
    pub detail: String,
}

impl RetroArchCheatSetupError {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for RetroArchCheatSetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for RetroArchCheatSetupError {}

impl From<PatchManagerError> for RetroArchCheatSetupError {
    fn from(error: PatchManagerError) -> Self {
        let code = match error {
            PatchManagerError::Catalogue(_) => "database_unavailable",
            PatchManagerError::Discovery(_) => "retroarch_discovery_failed",
            _ => "setup_preview_failed",
        };
        Self::new(code, error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroArchCheatSetupPlannedAction {
    InstallNew,
    AlreadyInstalled,
    ReplaceDifferent,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchCheatSetupPlannedEntry {
    pub display_title: String,
    pub platform: Option<String>,
    pub source_cheat_path: EncodedPath,
    pub destination_cheat_path: Option<EncodedPath>,
    pub planned_action: RetroArchCheatSetupPlannedAction,
    pub match_confidence: CheatMatchConfidence,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetroArchCheatSetupPreviewSummary {
    pub archivefs_game_records_examined: usize,
    pub cheat_records_discovered: usize,
    pub exact_matches: usize,
    pub strong_matches: usize,
    pub weak_matches: usize,
    pub ambiguous_matches: usize,
    pub eligible_new_installations: usize,
    pub already_installed: usize,
    pub different_existing_files: usize,
    pub replacement_blocked: usize,
    pub conflicts: usize,
    pub malformed_or_skipped_entries: usize,
    pub total_writes_proposed: usize,
    pub total_backups_proposed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchCheatSetupPreview {
    pub summary: RetroArchCheatSetupPreviewSummary,
    pub planned_entries: Vec<RetroArchCheatSetupPlannedEntry>,
    pub journal_path: CheatInstallPath,
}

/// Installer-ready setup plan. The public preview is stable structured data;
/// `installer_entries` are the existing matcher/stager outputs passed directly
/// to the existing installer after CLI confirmation.
pub struct RetroArchCheatSetupPlan {
    pub selected_profile: RetroArchCheatSetupProfile,
    pub destination_root: PathBuf,
    pub preview: RetroArchCheatSetupPreview,
    pub installer_entries: Vec<CheatAvailabilityEntry>,
    pub warnings: Vec<RetroArchCheatSetupMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroArchCheatSetupStatus {
    Preview,
    Cancelled,
    Applied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetroArchCheatSetupNextStep {
    pub order: u32,
    pub action: String,
    pub detail: String,
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchCheatSetupResult {
    pub schema_version: u32,
    pub status: RetroArchCheatSetupStatus,
    pub selected_profile: Option<RetroArchCheatSetupProfile>,
    pub discovered_profiles: Vec<RetroArchCheatSetupProfile>,
    pub configuration_path: Option<EncodedPath>,
    pub cheat_destination_root: Option<EncodedPath>,
    pub catalogue_path: EncodedPath,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieved_source: Option<super::CheatSourceSetupContext>,
    pub database_path: EncodedPath,
    pub preview: Option<RetroArchCheatSetupPreview>,
    pub planned_entries: Vec<RetroArchCheatSetupPlannedEntry>,
    pub install_result: Option<CheatInstallRun>,
    pub journal_path: Option<CheatInstallPath>,
    pub warnings: Vec<RetroArchCheatSetupMessage>,
    pub errors: Vec<RetroArchCheatSetupError>,
    pub next_steps: Vec<RetroArchCheatSetupNextStep>,
}

impl RetroArchCheatSetupResult {
    pub fn failed(
        discovery: Option<&RetroArchCheatSetupDiscovery>,
        catalogue_path: &Path,
        database_path: &Path,
        error: RetroArchCheatSetupError,
    ) -> Self {
        let next_step = match error.code.as_str() {
            "profile_selection_required" => (
                "select_profile",
                "Choose one eligible discovered profile by its exact profile_id.",
            ),
            "no_eligible_profiles" | "profile_ineligible" => (
                "fix_retroarch_profile",
                "Review profile blockers, launch RetroArch once, and configure a safe absolute cheat_database_path.",
            ),
            "database_unavailable" | "database_has_no_usable_records" => (
                "scan_archivefs_library",
                "Create or update the ArchiveFS library with the normal source/library scan workflow.",
            ),
            _ => (
                "review_error",
                "Review the structured error and correct the reported local input.",
            ),
        };
        Self {
            schema_version: RETROARCH_CHEAT_SETUP_SCHEMA_VERSION,
            status: RetroArchCheatSetupStatus::Failed,
            selected_profile: None,
            discovered_profiles: discovery
                .map(|value| value.profiles.clone())
                .unwrap_or_default(),
            configuration_path: None,
            cheat_destination_root: None,
            catalogue_path: EncodedPath::from_path(catalogue_path),
            retrieved_source: None,
            database_path: EncodedPath::from_path(database_path),
            preview: None,
            planned_entries: Vec::new(),
            install_result: None,
            journal_path: None,
            warnings: Vec::new(),
            errors: vec![error],
            next_steps: vec![RetroArchCheatSetupNextStep {
                order: 1,
                action: next_step.0.to_string(),
                detail: next_step.1.to_string(),
                command: None,
            }],
        }
    }
}

pub fn discover_retroarch_cheat_setup_profiles(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: &DiscoveryEnvironment,
    configuration_override: Option<&Path>,
) -> Result<RetroArchCheatSetupDiscovery, RetroArchCheatSetupError> {
    let report = discover_retroarch_environment(filesystem, environment).map_err(|error| {
        RetroArchCheatSetupError::new("retroarch_discovery_failed", error.to_string())
    })?;
    let profiles = report
        .profiles
        .iter()
        .map(|profile| classify_profile(profile, configuration_override))
        .collect();
    Ok(RetroArchCheatSetupDiscovery {
        schema_version: RETROARCH_CHEAT_SETUP_SCHEMA_VERSION,
        profiles,
        diagnostics: report.diagnostics.clone(),
        environment: report,
    })
}

pub fn resolve_retroarch_cheat_setup_profile(
    discovery: &RetroArchCheatSetupDiscovery,
    profile_id: Option<&str>,
) -> Result<RetroArchCheatSetupProfile, RetroArchCheatSetupError> {
    if let Some(profile_id) = profile_id {
        let profile = discovery
            .profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
            .ok_or_else(|| {
                RetroArchCheatSetupError::new(
                    "unknown_profile_id",
                    format!("no discovered RetroArch profile has ID '{profile_id}'"),
                )
            })?;
        if !profile.eligible {
            let reasons = profile
                .blockers
                .iter()
                .map(|blocker| blocker.code.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(RetroArchCheatSetupError::new(
                "profile_ineligible",
                format!("profile '{profile_id}' is not usable: {reasons}"),
            ));
        }
        return Ok(profile.clone());
    }

    let eligible = discovery
        .profiles
        .iter()
        .filter(|profile| profile.eligible)
        .collect::<Vec<_>>();
    match eligible.as_slice() {
        [profile] => Ok((*profile).clone()),
        [] => Err(RetroArchCheatSetupError::new(
            "no_eligible_profiles",
            "no discovered RetroArch profile has a safe, resolved cheats destination",
        )),
        _ => Err(RetroArchCheatSetupError::new(
            "profile_selection_required",
            format!(
                "{} eligible RetroArch profiles were discovered; select one by exact profile ID",
                eligible.len()
            ),
        )),
    }
}

pub fn build_retroarch_cheat_setup_plan(
    filesystem: &dyn ReadOnlyHostFilesystem,
    discovery: &RetroArchCheatSetupDiscovery,
    selected_profile: &RetroArchCheatSetupProfile,
    catalogue_path: &Path,
    database_path: &Path,
    journal_path: &Path,
    allow_replace_different: bool,
) -> Result<RetroArchCheatSetupPlan, RetroArchCheatSetupError> {
    if !selected_profile.eligible {
        return Err(RetroArchCheatSetupError::new(
            "profile_ineligible",
            "the selected RetroArch profile is not eligible",
        ));
    }
    let destination = selected_profile
        .cheat_destination_root
        .as_ref()
        .filter(|path| !path.lossy)
        .ok_or_else(|| {
            RetroArchCheatSetupError::new(
                "cheats_destination_unresolved",
                "the selected profile has no lossless resolved cheats destination",
            )
        })?;
    let destination_root = PathBuf::from(&destination.display);

    let catalogue_games = load_catalogue_evidence_read_only(database_path)?;
    let usable_game_records = catalogue_games
        .iter()
        .filter(|game| game.is_present)
        .count();
    if usable_game_records == 0 {
        return Err(RetroArchCheatSetupError::new(
            "database_has_no_usable_records",
            "the ArchiveFS database contains no game records; run an ArchiveFS library scan first",
        ));
    }
    let archives = load_retroarch_catalogue_archives_read_only(database_path)?;

    let environment_profile = discovery
        .environment
        .profiles
        .iter()
        .find(|profile| profile_matches_setup_profile(profile, selected_profile))
        .cloned()
        .ok_or_else(|| {
            RetroArchCheatSetupError::new(
                "selected_profile_disappeared",
                "the selected profile could not be mapped back to its lossless discovery record",
            )
        })?;
    let selected_environment = RetroArchEnvironmentReport {
        format_version: discovery.environment.format_version,
        profiles: vec![environment_profile],
        diagnostics: discovery.environment.diagnostics.clone(),
    };
    let advisory = build_retroarch_advisory_plan(filesystem, selected_environment, archives);
    let snapshot = load_cheat_catalogue_snapshot(filesystem, "local-catalogue", catalogue_path);
    if !snapshot.complete && snapshot.games.is_empty() {
        let codes = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(RetroArchCheatSetupError::new(
            "unsupported_catalogue",
            format!("the local catalogue could not be loaded safely: {codes}"),
        ));
    }

    let availability = build_cheat_availability_report(
        filesystem,
        &snapshot,
        &catalogue_games,
        Some(&advisory),
        Some(&destination_root),
    );
    let install_preview =
        plan_cheat_install_entries(&availability.entries, allow_replace_different);

    let planned_entries = availability
        .entries
        .iter()
        .map(|entry| planned_entry(entry, allow_replace_different))
        .collect::<Vec<_>>();
    let different_existing_files = availability
        .entries
        .iter()
        .filter(|entry| entry.staging_plan.planned_action == CheatStagingAction::ReplaceDifferent)
        .count();
    let skipped = availability
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.staging_plan.planned_action,
                CheatStagingAction::NotEligible | CheatStagingAction::Conflict
            )
        })
        .count();
    let summary = RetroArchCheatSetupPreviewSummary {
        archivefs_game_records_examined: usable_game_records,
        cheat_records_discovered: snapshot.games.len(),
        exact_matches: availability.summary.exact_matches,
        strong_matches: availability.summary.strong_matches,
        weak_matches: availability.summary.weak_matches,
        ambiguous_matches: availability.summary.ambiguous_matches,
        eligible_new_installations: availability.summary.not_installed,
        already_installed: availability.summary.already_installed,
        different_existing_files,
        replacement_blocked: if allow_replace_different {
            0
        } else {
            different_existing_files
        },
        conflicts: availability.summary.conflicts,
        malformed_or_skipped_entries: skipped.saturating_add(snapshot.diagnostics.len()),
        total_writes_proposed: install_preview
            .iter()
            .filter(|entry| {
                entry.write_required
                    && matches!(
                        entry.outcome,
                        CheatInstallOutcome::InstalledNew | CheatInstallOutcome::ReplacedWithBackup
                    )
            })
            .count(),
        total_backups_proposed: install_preview
            .iter()
            .filter(|entry| {
                entry.write_required && entry.outcome == CheatInstallOutcome::ReplacedWithBackup
            })
            .count(),
    };
    let mut warnings = Vec::new();
    if !snapshot.complete {
        warnings.push(RetroArchCheatSetupMessage {
            code: "catalogue_incomplete".to_string(),
            detail: "one or more catalogue entries could not be read safely and were skipped"
                .to_string(),
        });
    }
    if different_existing_files > 0 && !allow_replace_different {
        warnings.push(RetroArchCheatSetupMessage {
            code: "replacement_not_allowed".to_string(),
            detail: format!(
                "{different_existing_files} different existing cheat file(s) will be left untouched; use --replace-different to permit verified backup and replacement"
            ),
        });
    }

    Ok(RetroArchCheatSetupPlan {
        selected_profile: selected_profile.clone(),
        destination_root,
        preview: RetroArchCheatSetupPreview {
            summary,
            planned_entries,
            journal_path: CheatInstallPath::from_path(journal_path),
        },
        installer_entries: availability.entries,
        warnings,
    })
}

fn classify_profile(
    profile: &RetroArchProfile,
    configuration_override: Option<&Path>,
) -> RetroArchCheatSetupProfile {
    let mut blockers = Vec::new();
    let mut executable_evidence = profile.evidence.executables.clone();
    executable_evidence.extend(profile.app_images.iter().map(|image| image.path.clone()));

    let installation_verified = match profile.profile_kind {
        ProfileKind::Native => {
            !profile.evidence.executables.is_empty()
                || profile.app_images.iter().any(usable_app_image_evidence)
        }
        ProfileKind::AppImage => profile.app_images.iter().any(usable_app_image_evidence),
        ProfileKind::Flatpak => profile.evidence.flatpak_metadata_found,
    };
    if !installation_verified {
        push_blocker(
            &mut blockers,
            "installation_not_verified",
            "no sufficiently strong executable or installation evidence was found",
        );
    }

    let configuration_path = profile.config_file.path.clone();
    if configuration_path.lossy {
        push_blocker(
            &mut blockers,
            "configuration_path_lossy",
            "the configuration path cannot be represented losslessly for selection",
        );
    }
    if let Some(configuration_override) = configuration_override {
        let override_path = EncodedPath::from_path(configuration_override);
        if override_path.lossy
            || configuration_path.lossy
            || override_path.display != configuration_path.display
        {
            push_blocker(
                &mut blockers,
                "configuration_override_mismatch",
                "this profile does not use the exact --config path",
            );
        }
    }
    match &profile.config_file.read {
        ConfigReadOutcome::Parsed {
            malformed_lines,
            include_detected,
            complete,
        } if *complete && !*include_detected && malformed_lines.is_empty() => {}
        ConfigReadOutcome::Parsed { .. } => push_blocker(
            &mut blockers,
            "configuration_ambiguous",
            "the RetroArch configuration is incomplete, includes another file, or has malformed lines",
        ),
        ConfigReadOutcome::NotAttempted => push_blocker(
            &mut blockers,
            config_probe_blocker(profile.config_file.probe),
            "the RetroArch configuration could not be read",
        ),
        ConfigReadOutcome::TooLarge { .. } => push_blocker(
            &mut blockers,
            "configuration_too_large",
            "the RetroArch configuration exceeds the bounded read limit",
        ),
        ConfigReadOutcome::InvalidUtf8 => push_blocker(
            &mut blockers,
            "configuration_invalid_utf8",
            "the RetroArch configuration is not valid UTF-8",
        ),
    }

    let cheats = profile
        .paths
        .iter()
        .find(|finding| finding.purpose == PathPurpose::Cheats);
    let cheat_destination_root = cheats.and_then(|finding| finding.resolved_path.clone());
    match cheats {
        None => push_blocker(
            &mut blockers,
            "cheats_destination_unresolved",
            "discovery did not report a cheat_database_path finding",
        ),
        Some(finding) if finding.resolution != ResolutionState::ConfiguredResolved => {
            push_blocker(
                &mut blockers,
                "cheats_destination_unresolved",
                "cheat_database_path is missing, empty, relative, or otherwise unresolved",
            );
        }
        Some(finding) => match &finding.resolved_path {
            None => push_blocker(
                &mut blockers,
                "cheats_destination_unresolved",
                "cheat_database_path has no resolved path",
            ),
            Some(path) if path.lossy => push_blocker(
                &mut blockers,
                "cheats_destination_lossy",
                "the cheats destination cannot be reconstructed from its display form",
            ),
            Some(path) => {
                if let Err(error) = validate_destination_root(Path::new(&path.display)) {
                    push_blocker(
                        &mut blockers,
                        "cheats_destination_unsafe",
                        &error.to_string(),
                    );
                }
            }
        },
    }

    let profile_id = stable_profile_id(profile);
    let eligible = blockers.is_empty();
    RetroArchCheatSetupProfile {
        profile_id,
        installation_type: profile.profile_kind,
        scope: profile.scope,
        state: if eligible {
            RetroArchCheatSetupProfileState::Eligible
        } else {
            RetroArchCheatSetupProfileState::Ineligible
        },
        eligible,
        executable_evidence,
        configuration_path,
        cheat_destination_root,
        blockers,
        diagnostics: profile.diagnostics.clone(),
    }
}

fn usable_app_image_evidence(
    candidate: &crate::emulator_environment::retroarch::AppImageCandidate,
) -> bool {
    candidate.probe == FsProbe::PresentFile
        && candidate.executable == Some(ExecutableState::Executable)
        && matches!(
            candidate.confidence,
            AppImageIdentificationConfidence::Exact | AppImageIdentificationConfidence::Strong
        )
        && !matches!(
            candidate.config_association,
            ConfigAssociation::Unknown | ConfigAssociation::Ambiguous
        )
}

fn stable_profile_id(profile: &RetroArchProfile) -> String {
    let kind = match profile.profile_kind {
        ProfileKind::Native => "native",
        ProfileKind::AppImage => "appimage",
        ProfileKind::Flatpak => "flatpak",
    };
    let scope = match profile.scope {
        ProfileScope::User => "user",
        ProfileScope::System => "system",
    };
    if profile.config_file.path.lossy {
        return format!("{kind}-{scope}-unusable-lossy-config");
    }
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(profile.config_file.path.display.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{kind}-{scope}-{suffix}")
}

fn config_probe_blocker(probe: FsProbe) -> &'static str {
    match probe {
        FsProbe::Missing => "configuration_missing",
        FsProbe::Inaccessible | FsProbe::IoError => "configuration_inaccessible",
        FsProbe::Symlink => "configuration_symlink_rejected",
        FsProbe::WrongType | FsProbe::PresentDirectory => "configuration_wrong_type",
        FsProbe::PresentFile => "configuration_unreadable",
    }
}

fn push_blocker(blockers: &mut Vec<RetroArchCheatSetupProfileBlocker>, code: &str, detail: &str) {
    blockers.push(RetroArchCheatSetupProfileBlocker {
        code: code.to_string(),
        detail: detail.to_string(),
    });
}

fn profile_matches_setup_profile(
    profile: &RetroArchProfile,
    setup: &RetroArchCheatSetupProfile,
) -> bool {
    profile.profile_kind == setup.installation_type
        && profile.scope == setup.scope
        && stable_profile_id(profile) == setup.profile_id
}

fn planned_entry(
    entry: &CheatAvailabilityEntry,
    allow_replace_different: bool,
) -> RetroArchCheatSetupPlannedEntry {
    let (planned_action, reason) = match entry.staging_plan.planned_action {
        CheatStagingAction::InstallNew => (
            RetroArchCheatSetupPlannedAction::InstallNew,
            entry.staging_plan.reason.to_string(),
        ),
        CheatStagingAction::AlreadyInstalled => (
            RetroArchCheatSetupPlannedAction::AlreadyInstalled,
            entry.staging_plan.reason.to_string(),
        ),
        CheatStagingAction::ReplaceDifferent if allow_replace_different => (
            RetroArchCheatSetupPlannedAction::ReplaceDifferent,
            entry.staging_plan.reason.to_string(),
        ),
        CheatStagingAction::ReplaceDifferent => (
            RetroArchCheatSetupPlannedAction::Skipped,
            "replace_different_requires_option".to_string(),
        ),
        CheatStagingAction::Conflict | CheatStagingAction::NotEligible => (
            RetroArchCheatSetupPlannedAction::Skipped,
            entry.staging_plan.reason.to_string(),
        ),
    };
    RetroArchCheatSetupPlannedEntry {
        display_title: entry.game.source_game_name.clone(),
        platform: entry.game.source_platform.clone(),
        source_cheat_path: entry.staging_plan.source_cheat_path.clone(),
        destination_cheat_path: entry.staging_plan.proposed_destination_path.clone(),
        planned_action,
        match_confidence: entry.game_match.confidence,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator_environment::HostReadOnlyFilesystem;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "archivefs-cheat-setup-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn environment(&self, path: Option<PathBuf>) -> DiscoveryEnvironment {
            DiscoveryEnvironment {
                home: Some(self.root.as_os_str().to_os_string()),
                xdg_config_home: Some(self.root.join("config").into_os_string()),
                path: path.map(|path| path.into_os_string()),
                user_flatpak_root: self.root.join("user-flatpak"),
                system_flatpak_root: self.root.join("system-flatpak"),
                app_image_search_roots: vec![self.root.join("Applications")],
                desktop_file_roots: vec![self.root.join("applications")],
            }
        }

        fn native(&self) -> DiscoveryEnvironment {
            let bin = self.root.join("bin");
            let config = self.root.join("config/retroarch/retroarch.cfg");
            let cheats = self.root.join("retroarch-cheats");
            fs::create_dir_all(&bin).unwrap();
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            fs::create_dir_all(&cheats).unwrap();
            fs::write(
                &config,
                format!("cheat_database_path = \"{}\"\n", cheats.display()),
            )
            .unwrap();
            let executable = bin.join("retroarch");
            fs::write(&executable, b"test executable").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            }
            self.environment(Some(bin))
        }

        fn add_flatpak(&self) {
            let config = self
                .root
                .join(".var/app/org.libretro.RetroArch/config/retroarch/retroarch.cfg");
            let cheats = self.root.join("flatpak-cheats");
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            fs::create_dir_all(&cheats).unwrap();
            fs::write(
                &config,
                format!("cheat_database_path = \"{}\"\n", cheats.display()),
            )
            .unwrap();
            fs::create_dir_all(self.root.join("user-flatpak/app/org.libretro.RetroArch")).unwrap();
        }

        fn add_distinct_appimage(&self) {
            let applications = self.root.join("Applications");
            let desktop_root = self.root.join("applications");
            fs::create_dir_all(&applications).unwrap();
            fs::create_dir_all(&desktop_root).unwrap();
            let appimage = applications.join("RetroArch.AppImage");
            fs::write(&appimage, b"appimage").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&appimage, fs::Permissions::from_mode(0o755)).unwrap();
            }
            fs::write(
                desktop_root.join("retroarch.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={}\n",
                    appimage.display()
                ),
            )
            .unwrap();
            let portable_config_home = PathBuf::from(format!("{}.config", appimage.display()));
            let config = portable_config_home.join("retroarch/retroarch.cfg");
            let cheats = self.root.join("appimage-cheats");
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            fs::create_dir_all(&cheats).unwrap();
            fs::write(
                &config,
                format!("cheat_database_path = \"{}\"\n", cheats.display()),
            )
            .unwrap();
        }

        fn add_shared_appimage(&self) {
            let applications = self.root.join("Applications");
            let desktop_root = self.root.join("applications");
            fs::create_dir_all(&applications).unwrap();
            fs::create_dir_all(&desktop_root).unwrap();
            let appimage = applications.join("RetroArch-shared.AppImage");
            fs::write(&appimage, b"appimage").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&appimage, fs::Permissions::from_mode(0o755)).unwrap();
            }
            fs::write(
                desktop_root.join("retroarch-shared.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={}\n",
                    appimage.display()
                ),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn exactly_one_native_profile_is_auto_selected() {
        let fixture = Fixture::new("native");
        let environment = fixture.native();
        let discovery =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        let selected = resolve_retroarch_cheat_setup_profile(&discovery, None).unwrap();
        assert_eq!(selected.installation_type, ProfileKind::Native);
        assert!(selected.eligible);
    }

    #[test]
    fn exactly_one_flatpak_profile_is_auto_selected() {
        let fixture = Fixture::new("flatpak");
        fixture.add_flatpak();
        let environment = fixture.environment(None);
        let discovery =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        let selected = resolve_retroarch_cheat_setup_profile(&discovery, None).unwrap();
        assert_eq!(selected.installation_type, ProfileKind::Flatpak);
        assert_eq!(selected.scope, ProfileScope::User);
    }

    #[test]
    fn distinct_verified_appimage_profile_is_eligible() {
        let fixture = Fixture::new("appimage");
        fixture.add_distinct_appimage();
        let environment = fixture.environment(None);
        let discovery =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        let selected = resolve_retroarch_cheat_setup_profile(&discovery, None).unwrap();
        assert_eq!(selected.installation_type, ProfileKind::AppImage);
    }

    #[test]
    fn appimage_sharing_native_config_is_deduplicated_into_native_profile() {
        let fixture = Fixture::new("appimage-shared");
        let environment = fixture.native();
        fixture.add_shared_appimage();
        let discovery =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        assert!(
            discovery
                .profiles
                .iter()
                .all(|profile| profile.installation_type != ProfileKind::AppImage)
        );
        let native = resolve_retroarch_cheat_setup_profile(&discovery, None).unwrap();
        assert_eq!(native.installation_type, ProfileKind::Native);
        assert!(
            native
                .executable_evidence
                .iter()
                .any(|path| path.display.ends_with("RetroArch-shared.AppImage"))
        );
    }

    #[test]
    fn multiple_profiles_require_exact_selection_and_unknown_id_is_rejected() {
        let fixture = Fixture::new("multiple");
        let environment = fixture.native();
        fixture.add_flatpak();
        let discovery =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        assert_eq!(
            resolve_retroarch_cheat_setup_profile(&discovery, None)
                .unwrap_err()
                .code,
            "profile_selection_required"
        );
        assert_eq!(
            resolve_retroarch_cheat_setup_profile(&discovery, Some("native"))
                .unwrap_err()
                .code,
            "unknown_profile_id"
        );
        let exact_id = discovery
            .profiles
            .iter()
            .find(|profile| profile.eligible)
            .unwrap()
            .profile_id
            .clone();
        assert_eq!(
            resolve_retroarch_cheat_setup_profile(&discovery, Some(&exact_id))
                .unwrap()
                .profile_id,
            exact_id
        );
    }

    #[test]
    fn missing_config_is_visible_but_ineligible() {
        let fixture = Fixture::new("missing-config");
        let bin = fixture.root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join("retroarch");
        fs::write(&executable, b"retroarch").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let discovery = discover_retroarch_cheat_setup_profiles(
            &HostReadOnlyFilesystem,
            &fixture.environment(Some(bin)),
            None,
        )
        .unwrap();
        let native = discovery
            .profiles
            .iter()
            .find(|profile| profile.installation_type == ProfileKind::Native)
            .unwrap();
        assert!(!native.eligible);
        assert!(
            native
                .blockers
                .iter()
                .any(|blocker| blocker.code == "configuration_missing")
        );
        assert_eq!(
            resolve_retroarch_cheat_setup_profile(&discovery, Some(&native.profile_id))
                .unwrap_err()
                .code,
            "profile_ineligible"
        );
    }

    #[test]
    fn incomplete_including_config_is_ambiguous_and_override_filters_exactly() {
        let fixture = Fixture::new("ambiguous-config");
        let environment = fixture.native();
        fixture.add_flatpak();
        let native_config = fixture.root.join("config/retroarch/retroarch.cfg");
        fs::write(
            &native_config,
            format!(
                "#include \"other.cfg\"\ncheat_database_path = \"{}\"\n",
                fixture.root.join("retroarch-cheats").display()
            ),
        )
        .unwrap();
        let discovery = discover_retroarch_cheat_setup_profiles(
            &HostReadOnlyFilesystem,
            &environment,
            Some(&native_config),
        )
        .unwrap();
        let native = discovery
            .profiles
            .iter()
            .find(|profile| profile.installation_type == ProfileKind::Native)
            .unwrap();
        assert!(
            native
                .blockers
                .iter()
                .any(|blocker| blocker.code == "configuration_ambiguous")
        );
        let flatpak = discovery
            .profiles
            .iter()
            .find(|profile| profile.installation_type == ProfileKind::Flatpak)
            .unwrap();
        assert!(
            flatpak
                .blockers
                .iter()
                .any(|blocker| blocker.code == "configuration_override_mismatch")
        );
    }

    #[cfg(unix)]
    #[test]
    fn cheat_destination_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("symlink");
        let environment = fixture.native();
        let cheats = fixture.root.join("retroarch-cheats");
        fs::remove_dir(&cheats).unwrap();
        symlink(fixture.root.join("elsewhere"), &cheats).unwrap();
        let discovery =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        let native = discovery
            .profiles
            .iter()
            .find(|profile| profile.installation_type == ProfileKind::Native)
            .unwrap();
        assert!(
            native
                .blockers
                .iter()
                .any(|blocker| blocker.code == "cheats_destination_unsafe")
        );
    }

    #[test]
    fn discovery_order_and_ids_are_deterministic_and_create_nothing() {
        let fixture = Fixture::new("deterministic");
        let environment = fixture.native();
        fixture.add_flatpak();
        let before = snapshot_paths(&fixture.root);
        let first =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        let second =
            discover_retroarch_cheat_setup_profiles(&HostReadOnlyFilesystem, &environment, None)
                .unwrap();
        assert_eq!(
            first
                .profiles
                .iter()
                .map(|profile| &profile.profile_id)
                .collect::<Vec<_>>(),
            second
                .profiles
                .iter()
                .map(|profile| &profile.profile_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(snapshot_paths(&fixture.root), before);
    }

    fn snapshot_paths(root: &Path) -> Vec<PathBuf> {
        fn visit(root: &Path, paths: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(root) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                paths.push(path.clone());
                if path.is_dir() {
                    visit(&path, paths);
                }
            }
        }
        let mut paths = Vec::new();
        visit(root, &mut paths);
        paths.sort();
        paths
    }
}
