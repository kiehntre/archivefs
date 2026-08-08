//! The Canonical Organisation page.
//!
//! Configures the master ROM root, chooses an organisation mode, collects
//! candidate ROM files, resolves each candidate's platform identity from the
//! database and its canonical RomM slug from the identity cache, builds a
//! read-only plan, and - only after an explicit typed confirmation - applies
//! the approved subset through the shared journaled engine. Rollback restores
//! the prior state.
//!
//! The page states loudly that planning changes nothing until the user
//! approves, and never offers Apply for conflicts, blocked or unknown entries.

use std::collections::BTreeSet;
use std::path::PathBuf;

use archivefs_core::dat::rom_organisation::*;
use archivefs_core::identity_source::cache::{IdentityCacheLocation, load_cache};
use archivefs_core::identity_source::model::IdentityProvider;
use archivefs_core::identity_source::settings::default_identity_root;
use archivefs_core::platform::identity::{PlatformIdentityResolution, resolve_platform_identity};
use archivefs_core::{
    Config, Database, clear_master_rom_root_default, default_database_path,
    set_master_rom_root_default,
};
use eframe::egui;

use crate::ui::{components as widgets, theme};

/// The page's authoritative state.
pub(crate) struct RomOrganisationPageState {
    master_root_draft: String,
    saved_master_root: Option<PathBuf>,
    master_root_error: Option<String>,
    mode: OrganisationMode,
    /// Candidate source files collected from the configured source folders.
    sources: Vec<PathBuf>,
    /// The read-only plan, when generated. `generation` is bumped on every
    /// regeneration so a stale review decision can never apply.
    plan: Option<OrganisationPlan>,
    plan_generation: u64,
    /// Source paths the user has approved (checked) for apply.
    approved: BTreeSet<String>,
    filter: Option<OrganisationStatus>,
    applied: Option<archivefs_core::dat::rename_apply::RenameTransaction>,
    applied_journal: PathBuf,
    result_message: Option<String>,
    error: Option<String>,
    /// Set when the user asked to apply; holds the count awaiting a typed
    /// confirmation for large batches.
    pending_apply: Option<usize>,
    confirm_text: String,
}

/// Batches larger than this require typing the exact confirmation phrase
/// before any mutation happens (the same philosophy as DAT rename apply).
pub(crate) const TYPED_CONFIRMATION_THRESHOLD: usize = 8;

/// The exact phrase a user must type to confirm a large apply, with wording
/// that is truthful for the chosen mode.
pub(crate) fn apply_confirmation_phrase(mode: OrganisationMode, count: usize) -> String {
    match mode {
        OrganisationMode::RenameInPlace => format!("RENAME {count} FILES"),
        OrganisationMode::MoveRealFile | OrganisationMode::OrganiseSymlinkOnly => {
            format!("MOVE {count} FILES")
        }
    }
}

impl Default for RomOrganisationPageState {
    fn default() -> Self {
        Self {
            master_root_draft: String::new(),
            saved_master_root: None,
            master_root_error: None,
            mode: OrganisationMode::MoveRealFile,
            sources: Vec::new(),
            plan: None,
            plan_generation: 0,
            approved: BTreeSet::new(),
            filter: None,
            applied: None,
            applied_journal:
                archivefs_core::dat::rename_apply::journal::default_rename_transaction_dir()
                    .unwrap_or_else(|_| PathBuf::from("rename-transactions")),
            result_message: None,
            error: None,
            pending_apply: None,
            confirm_text: String::new(),
        }
    }
}

impl RomOrganisationPageState {
    /// Loads the configured master root and scans the configured source
    /// folders for candidate files (bounded). Read-only.
    pub(crate) fn load() -> Self {
        let mut state = Self::default();
        if let Ok(config) = Config::load_default() {
            state.saved_master_root = config.master_rom_root.clone();
            state.master_root_draft = config
                .master_rom_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            state.sources = collect_source_files(&config.source_folders);
        }
        state
    }

    pub(crate) fn set_mode(&mut self, mode: OrganisationMode) {
        self.mode = mode;
        self.plan = None;
    }

    /// Saves the master ROM root draft (or clears it). Configuring a root
    /// never moves anything by itself.
    pub(crate) fn save_master_root(&mut self) {
        let trimmed = self.master_root_draft.trim();
        if trimmed.is_empty() {
            match clear_master_rom_root_default() {
                Ok(_) => {
                    self.saved_master_root = None;
                    self.master_root_error = None;
                }
                Err(error) => self.master_root_error = Some(error.to_string()),
            }
            return;
        }
        let path = PathBuf::from(trimmed);
        match set_master_rom_root_default(&path) {
            Ok(_) => {
                self.saved_master_root = Some(path);
                self.master_root_error = None;
            }
            Err(error) => self.master_root_error = Some(error.to_string()),
        }
    }

    /// Re-scans the configured source folders for candidate files.
    pub(crate) fn rescan_sources(&mut self) {
        if let Ok(config) = Config::load_default() {
            self.sources = collect_source_files(&config.source_folders);
        }
        self.plan = None;
        self.approved.clear();
    }

    /// Builds a fresh read-only plan from the current candidates. Every
    /// rebuild bumps the generation, so any earlier review decision is stale.
    pub(crate) fn generate_plan(&mut self) {
        self.plan_generation += 1;
        let Some(master_root) = self.saved_master_root.clone() else {
            self.error = Some("configure a master ROM root first".to_string());
            self.plan = None;
            return;
        };
        let Some(candidates) = build_candidates(&self.sources, self.plan_generation) else {
            self.error = Some("could not read the platform identity database".to_string());
            self.plan = None;
            return;
        };
        let slug_map = load_slug_map();
        let plan = build_organisation_plan(&OrganisationPlanRequest {
            master_root: &master_root,
            mode: self.mode,
            candidates: &candidates,
            slug_for_platform: &|platform| slug_map.get(platform).cloned(),
            generation: self.plan_generation,
        });
        self.approved = plan
            .suggested()
            .map(|entry| entry.source_path.to_string_lossy().into_owned())
            .collect();
        self.plan = Some(plan);
        self.error = None;
    }

    pub(crate) fn toggle_approved(&mut self, source: &str) {
        if !self.approved.remove(source) {
            self.approved.insert(source.to_string());
        }
    }

    pub(crate) fn set_filter(&mut self, filter: Option<OrganisationStatus>) {
        self.filter = filter;
    }

    /// Applies the approved Suggested entries after the caller has confirmed.
    pub(crate) fn apply(&mut self) {
        let Some(plan) = &self.plan else {
            return;
        };
        let Some(master_root) = self.saved_master_root.clone() else {
            return;
        };
        let approved = self.approved.clone();
        let mut transaction = match build_organisation_transaction(plan, &approved, plan.generation)
        {
            Ok(transaction) => transaction,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let journal = self.applied_journal.clone();
        std::fs::create_dir_all(&journal).ok();
        let mut trusted_roots = vec![master_root.clone()];
        for source in &self.sources {
            if let Some(parent) = source.parent()
                && let Ok(canonical) = std::fs::canonicalize(parent)
            {
                trusted_roots.push(canonical);
            }
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        match archivefs_core::dat::rom_organisation::apply_organisation_transaction(
            &mut transaction,
            &approved,
            plan.generation,
            archivefs_core::safe_read::TrustedRoots::from_paths(trusted_roots),
            &journal,
            cancel.as_ref(),
            self.mode,
        ) {
            Ok(outcome) => {
                self.applied = Some(outcome.transaction.clone());
                self.result_message = Some(format!(
                    "Applied {} organisation(s). Roll back is available for this transaction.",
                    outcome.transaction.applied_count()
                ));
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Rolls back the last applied transaction.
    pub(crate) fn rollback(&mut self) {
        let Some(mut transaction) = self.applied.take() else {
            return;
        };
        let journal = self.applied_journal.clone();
        let Some(master_root) = self.saved_master_root.clone() else {
            self.applied = Some(transaction);
            return;
        };
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        match archivefs_core::dat::rom_organisation::rollback_organisation_transaction(
            &mut transaction,
            &journal,
            cancel.as_ref(),
            &master_root,
        ) {
            Ok(outcome) => {
                let dirs = outcome.directories_removed.len();
                self.result_message = Some(format!(
                    "Rolled back organisation; {} empty platform director(ies) removed.",
                    dirs
                ));
                self.plan = None;
                self.approved.clear();
            }
            Err(error) => {
                self.applied = Some(transaction);
                self.error = Some(error);
            }
        }
    }
}

/// Walks the configured source folders (bounded) and collects candidate file
/// paths. Read-only; never follows symlinked directories.
fn collect_source_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 4;
    const MAX_FILES: usize = 2_000;
    let mut out = Vec::new();
    for root in roots {
        let mut queue: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
        while let Some((dir, depth)) = queue.pop() {
            if out.len() >= MAX_FILES {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if out.len() >= MAX_FILES {
                    break;
                }
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.is_dir() {
                    if depth < MAX_DEPTH {
                        queue.push((path, depth + 1));
                    }
                } else if metadata.is_file() {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// Builds organisation candidates by resolving each source file's platform
/// identity from the database. Returns `None` when the database is unreadable.
fn build_candidates(sources: &[PathBuf], generation: u64) -> Option<Vec<OrganisationCandidate>> {
    let database_path = default_database_path().ok()?;
    let database = Database::open_read_only(&database_path).ok()?;
    let mut candidates = Vec::new();
    for source in sources {
        let resolution = match database
            .find_archive_id_by_absolute_path(source)
            .ok()
            .flatten()
        {
            Some(archive_id) => {
                let evidence = database
                    .current_platform_identity_evidence(archive_id, generation)
                    .ok()
                    .unwrap_or_default();
                resolve_platform_identity(generation, evidence)
            }
            None => PlatformIdentityResolution::Unknown { generation },
        };
        candidates.push(OrganisationCandidate {
            source_path: source.clone(),
            resolution,
            canonical_name: None,
        });
    }
    Some(candidates)
}

/// Loads the canonical RomM slug map from the imported identity cache, if any.
fn load_slug_map() -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let Ok(identity_root) = default_identity_root() else {
        return map;
    };
    let location = IdentityCacheLocation::new(&identity_root, IdentityProvider::Romm);
    let Ok(cache) = load_cache(&location, None) else {
        return map;
    };
    for platform in archivefs_core::platform::canonical_ids() {
        if let Some(slug) = cache.romm_slug_for_platform(platform) {
            map.insert(platform.to_string(), slug.to_string());
        }
    }
    map
}

/// Draws the page and returns the confirmation request when the user clicks
/// Apply (the caller must confirm before any mutation happens).
pub(crate) fn show_rom_organisation_page(ui: &mut egui::Ui, state: &mut RomOrganisationPageState) {
    widgets::page_header(
        ui,
        "Canonical organisation",
        "Plan (and only after your approval, apply) moving identified games into canonical \
         platform directories beneath your master ROM root.",
    );

    widgets::card(ui, |ui| {
        ui.label(egui::RichText::new("Master ROM root").strong());
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.master_root_draft);
            if widgets::action_button(ui, "Save", widgets::ActionStyle::Primary, true).clicked() {
                state.save_master_root();
            }
        });
        match &state.saved_master_root {
            Some(root) => ui.label(
                egui::RichText::new(format!("Configured: {}", root.display()))
                    .color(theme::muted(ui)),
            ),
            None => ui.label(
                egui::RichText::new("No master ROM root is configured.").color(theme::muted(ui)),
            ),
        };
        if let Some(error) = &state.master_root_error {
            ui.label(
                egui::RichText::new(error.as_str()).color(widgets::StatusTone::Blocked.color(ui)),
            );
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Setting a root never moves anything by itself: organisation always requires a \
                 plan and your explicit approval.",
            )
            .color(theme::muted(ui)),
        );
    });

    ui.add_space(8.0);
    widgets::card(ui, |ui| {
        ui.label(egui::RichText::new("Organisation mode").strong());
        for mode in [
            OrganisationMode::RenameInPlace,
            OrganisationMode::MoveRealFile,
            OrganisationMode::OrganiseSymlinkOnly,
        ] {
            let selected = state.mode == mode;
            if ui.radio(selected, mode.label()).clicked() {
                state.set_mode(mode);
            }
        }
        ui.label(
            egui::RichText::new("Modes are separate choices and are never combined.")
                .color(theme::muted(ui)),
        );
    });

    ui.add_space(8.0);
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Candidate ROM files").strong());
            if widgets::action_button(ui, "Rescan sources", widgets::ActionStyle::Secondary, true)
                .clicked()
            {
                state.rescan_sources();
            }
            if widgets::action_button(ui, "Generate plan", widgets::ActionStyle::Primary, true)
                .clicked()
            {
                state.generate_plan();
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "{} candidate file(s) collected.",
                state.sources.len()
            ))
            .color(theme::muted(ui)),
        );
    });

    if let Some(plan) = state.plan.clone() {
        ui.add_space(8.0);
        widgets::banner(
            ui,
            "Planning only",
            "No files will be moved until you explicitly approve an apply.",
            widgets::StatusTone::Info,
        );
        ui.add_space(6.0);
        show_plan(ui, &plan, state);
    }

    if let Some(message) = &state.result_message {
        ui.add_space(8.0);
        widgets::banner(ui, "Result", message, widgets::StatusTone::Success);
        if state.applied.is_some()
            && widgets::action_button(
                ui,
                "Roll back this organisation",
                widgets::ActionStyle::Secondary,
                true,
            )
            .clicked()
        {
            state.rollback();
        }
    }

    if let Some(error) = &state.error {
        ui.add_space(8.0);
        widgets::banner(ui, "Not applied", error, widgets::StatusTone::Blocked);
    }
}

fn show_plan(ui: &mut egui::Ui, plan: &OrganisationPlan, state: &mut RomOrganisationPageState) {
    let entries: Vec<&OrganisationPlanEntry> = plan
        .entries
        .iter()
        .filter(|entry| state.filter.is_none_or(|f| entry.status == f))
        .collect();

    ui.horizontal(|ui| {
        ui.label("Filter:");
        let all = state.filter.is_none();
        if ui.selectable_label(all, "All").clicked() {
            state.set_filter(None);
        }
        for status in [
            OrganisationStatus::Suggested,
            OrganisationStatus::AlreadyOrganised,
            OrganisationStatus::Conflict,
            OrganisationStatus::Blocked,
            OrganisationStatus::Unsupported,
        ] {
            let selected = state.filter == Some(status);
            if ui.selectable_label(selected, status.label()).clicked() {
                state.set_filter(if selected { None } else { Some(status) });
            }
        }
    });
    ui.add_space(4.0);

    for entry in &entries {
        ui.horizontal(|ui| {
            match entry.status {
                OrganisationStatus::Suggested => {
                    let mut approved = state
                        .approved
                        .contains(&entry.source_path.to_string_lossy().into_owned());
                    if ui.checkbox(&mut approved, "").changed() {
                        state.toggle_approved(&entry.source_path.to_string_lossy());
                    }
                }
                _ => {
                    ui.add_space(20.0);
                }
            }
            widgets::status_badge(ui, entry.status.label(), status_tone(entry.status));
            ui.label(
                egui::RichText::new(format!(
                    "{} → {}",
                    entry.source_path.display(),
                    entry.destination_path.display()
                ))
                .monospace(),
            );
            if !entry.platform_display_name.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}",
                        entry.platform_display_name, entry.platform_source
                    ))
                    .color(theme::muted(ui)),
                );
            }
            if let Some(reason) = &entry.reason {
                ui.label(
                    egui::RichText::new(format!("({reason})"))
                        .color(widgets::StatusTone::Blocked.color(ui))
                        .small(),
                );
            }
        });
    }

    let suggested = plan.suggested().count();
    ui.add_space(8.0);
    let approved = state.approved.len();
    let applyable = suggested > 0 && approved > 0;
    if widgets::action_button(
        ui,
        format!("Apply approved organisation ({approved})"),
        widgets::ActionStyle::Primary,
        applyable,
    )
    .clicked()
    {
        state.pending_apply = Some(approved);
        state.confirm_text.clear();
    }

    // Explicit confirmation before any mutation. Large batches require typing
    // the exact phrase (truthful for the mode); small ones a plain confirm.
    if let Some(count) = state.pending_apply {
        ui.add_space(6.0);
        widgets::card(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Apply {} approved organisation(s)? Nothing is written until you confirm.",
                    count
                ))
                .strong(),
            );
            if count >= TYPED_CONFIRMATION_THRESHOLD {
                let phrase = apply_confirmation_phrase(state.mode, count);
                ui.label(
                    egui::RichText::new(format!("Type '{phrase}' to confirm:"))
                        .color(theme::muted(ui)),
                );
                ui.text_edit_singleline(&mut state.confirm_text);
            }
            ui.horizontal(|ui| {
                let phrase_ok = count < TYPED_CONFIRMATION_THRESHOLD
                    || state.confirm_text.trim() == apply_confirmation_phrase(state.mode, count);
                if widgets::action_button(
                    ui,
                    "Confirm apply",
                    widgets::ActionStyle::Primary,
                    phrase_ok,
                )
                .clicked()
                {
                    state.pending_apply = None;
                    state.confirm_text.clear();
                    state.apply();
                }
                if widgets::action_button(ui, "Cancel", widgets::ActionStyle::Secondary, true)
                    .clicked()
                {
                    state.pending_apply = None;
                    state.confirm_text.clear();
                }
            });
        });
    }
}

fn status_tone(status: OrganisationStatus) -> widgets::StatusTone {
    match status {
        OrganisationStatus::Suggested => widgets::StatusTone::Success,
        OrganisationStatus::AlreadyOrganised => widgets::StatusTone::Active,
        OrganisationStatus::Conflict => widgets::StatusTone::Warning,
        OrganisationStatus::Blocked => widgets::StatusTone::Pending,
        OrganisationStatus::Unsupported => widgets::StatusTone::Blocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirmation_phrase_is_truthful_for_the_mode() {
        assert_eq!(
            apply_confirmation_phrase(OrganisationMode::RenameInPlace, 3),
            "RENAME 3 FILES"
        );
        assert_eq!(
            apply_confirmation_phrase(OrganisationMode::MoveRealFile, 42),
            "MOVE 42 FILES"
        );
        assert_eq!(
            apply_confirmation_phrase(OrganisationMode::OrganiseSymlinkOnly, 1),
            "MOVE 1 FILES"
        );
    }

    #[test]
    fn the_default_state_has_no_master_root_and_no_sources() {
        let state = RomOrganisationPageState::default();
        assert!(state.saved_master_root.is_none());
        assert!(state.sources.is_empty());
        assert_eq!(state.mode, OrganisationMode::MoveRealFile);
    }
}
