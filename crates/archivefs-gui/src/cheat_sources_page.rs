//! The Cheat Sources page: the nine registered sources, made visible and
//! editable.
//!
//! # Why this page exists
//!
//! The registry, its priorities and its per-platform overrides have all
//! shipped for a while, reachable only from `archivefs cheat-source …` -
//! and per-platform participation was reachable only by hand-editing
//! `~/.config/archivefs/cheat_sources.toml`. Everything here surfaces
//! behaviour that already exists. Nothing on this page adds a field to that
//! file, and nothing changes how it is resolved.
//!
//! # The view model
//!
//! Following `romm_source`, authoritative state is turned into
//! [`CheatSourcesPageView`] by a pure function and the drawing code only
//! draws. The properties worth testing here are about *what is said* -
//! that a disabled source is still listed and still in its resolved
//! position, that an entry this build cannot act on is shown rather than
//! hidden, that nothing claims the upstream content was reviewed - and
//! those are data questions, answerable without a frame buffer.
//!
//! # Two registries, on purpose
//!
//! [`CheatSourcesPageState`] holds a `saved` registry and a `draft` one.
//! Edits touch the draft; the file is written only when the user saves. The
//! difference between the two *is* the unsaved-change state, so "is this
//! dirty?" cannot drift from "would saving change anything?" - they are the
//! same comparison.

use std::path::PathBuf;

use archivefs_core::patch_manager::{
    CheatSourceEntry, CheatSourceRegistry, UnresolvedPreference, build_default_registry,
    load_cheat_sources_config_from, save_cheat_sources_config_to,
};
use eframe::egui;

use crate::ui::{components as widgets, theme};

/// How a built-in source is described, everywhere it is described.
///
/// ArchiveFS reviewed the address, the transport, the parser and the limits
/// for these sources. It has not read the cheats they publish, and six of
/// the nine carry community-submitted content. A bare "Reviewed" or
/// "Trusted" badge would assert something untrue, so the scope travels with
/// the label and this constant is the only place the wording lives.
pub(crate) const BUILT_IN_INTEGRATION_LABEL: &str =
    "Built-in integration — upstream content not reviewed";

/// The sentence shown once per page, under the built-in label.
pub(crate) const UPSTREAM_CONTENT_CAVEAT: &str = "ArchiveFS checked how each source is fetched and parsed. It has not reviewed the cheats or \
     patches they publish, and does not endorse them. Codes come from the upstream community.";

/// Priority reads backwards to most people, so it is never shown bare.
pub(crate) const ORDERING_EXPLANATION: &str =
    "Sources are consulted in priority order, lowest number first. 1 is consulted before 999.";

/// What a source can do, in one phrase, derived from its capabilities.
///
/// Capability flags are the honest source for this: "remote" plus
/// "download" is what actually distinguishes a source that fetches from one
/// that reads what is already on disk.
fn provider_kind_label(entry: &CheatSourceEntry) -> &'static str {
    let caps = &entry.spec.capabilities;
    match (caps.remote, caps.download, caps.install) {
        (true, true, true) => "Downloads and installs",
        (true, true, false) => "Downloads (read-only)",
        (false, _, true) => "Local, installs",
        (false, _, false) => "Local, read-only",
        (true, false, _) => "Remote, read-only",
    }
}

/// One platform toggle on a source's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlatformParticipationView {
    pub(crate) platform: String,
    pub(crate) participating: bool,
    /// The source is off at source level, so this toggle cannot make it
    /// contribute. Shown inactive with a reason rather than hidden, so the
    /// control does not appear to have been silently ignored.
    pub(crate) overridden_by_source_level: bool,
}

/// One source's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheatSourceRowView {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) emulator: String,
    pub(crate) provider_kind: &'static str,
    /// Rendered coverage: the listed platforms, or the all-platforms phrase.
    pub(crate) platform_coverage: String,
    pub(crate) enabled: bool,
    pub(crate) priority: u32,
    /// 1-based position among *enabled* sources, or `None` when disabled.
    /// The number users actually reason about.
    pub(crate) consulted_position: Option<usize>,
    pub(crate) trust_label: &'static str,
    pub(crate) description: String,
    pub(crate) platforms: Vec<PlatformParticipationView>,
    /// This row differs from what is on disk.
    pub(crate) changed: bool,
}

/// A preferences entry this build cannot act on, shown read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnresolvedRowView {
    pub(crate) detail: String,
    pub(crate) explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SaveState {
    Idle,
    Saved,
    Failed(String),
}

/// Everything the page draws, derived and ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheatSourcesPageView {
    pub(crate) rows: Vec<CheatSourceRowView>,
    pub(crate) unresolved: Vec<UnresolvedRowView>,
    /// Unsaved edits are pending.
    pub(crate) dirty: bool,
    pub(crate) config_path: PathBuf,
    pub(crate) save_state: SaveState,
    pub(crate) load_error: Option<String>,
    /// Plain-language summary of what saving would do.
    pub(crate) pending_consequences: Vec<String>,
}

/// One edit the page can ask for. Applied to the draft, never to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheatSourcesPageAction {
    SetEnabled {
        id: String,
        enabled: bool,
    },
    SetPriority {
        id: String,
        priority: u32,
    },
    SetPlatformParticipation {
        id: String,
        platform: String,
        participating: bool,
    },
    Save,
    Revert,
}

/// The page's authoritative state.
pub(crate) struct CheatSourcesPageState {
    config_path: PathBuf,
    /// What is on disk, as last read or last written.
    saved: CheatSourceRegistry,
    /// What the user has edited but not yet saved.
    draft: CheatSourceRegistry,
    load_error: Option<String>,
    save_state: SaveState,
}

impl CheatSourcesPageState {
    /// Loads preferences from `config_path`, falling back to built-in
    /// defaults when the file is absent or unreadable.
    ///
    /// A load failure is surfaced, not swallowed, and leaves the page in a
    /// read-only-safe state: the draft equals the defaults, so a save would
    /// not silently overwrite a file that failed to parse. Refusing to save
    /// in that case is enforced in [`Self::apply`].
    pub(crate) fn load(config_path: PathBuf) -> Self {
        let mut saved = build_default_registry();
        let mut load_error = None;
        match load_cheat_sources_config_from(&config_path) {
            Ok(cfg) => saved.apply_config(&cfg),
            Err(error) => load_error = Some(error.to_string()),
        }
        let draft = saved.clone();
        Self {
            config_path,
            saved,
            draft,
            load_error,
            save_state: SaveState::Idle,
        }
    }

    /// Whether the draft differs from what is on disk.
    ///
    /// Compared as serialised configuration rather than as registries,
    /// because that is exactly what a save would write: an edit that
    /// round-trips to the same document is genuinely not a change.
    pub(crate) fn is_dirty(&self) -> bool {
        self.draft.to_config() != self.saved.to_config()
    }

    /// Applies one action. Only [`CheatSourcesPageAction::Save`] touches disk.
    pub(crate) fn apply(&mut self, action: CheatSourcesPageAction) {
        match action {
            CheatSourcesPageAction::SetEnabled { id, enabled } => {
                if let Some(entry) = self.draft.get_mut(&id) {
                    entry.enabled = enabled;
                }
                self.save_state = SaveState::Idle;
            }
            CheatSourcesPageAction::SetPriority { id, priority } => {
                // Out-of-range values are refused by the editor before they
                // reach here (see `priority_editor`), matching the CLI, which
                // rejects rather than clamps so a confirmation never reports a
                // value the user did not ask for.
                if (MIN_PRIORITY..=MAX_PRIORITY).contains(&priority)
                    && let Some(entry) = self.draft.get_mut(&id)
                {
                    entry.priority = priority;
                }
                self.save_state = SaveState::Idle;
            }
            CheatSourcesPageAction::SetPlatformParticipation {
                id,
                platform,
                participating,
            } => {
                self.draft
                    .set_platform_participation(&id, &platform, participating);
                self.save_state = SaveState::Idle;
            }
            CheatSourcesPageAction::Revert => {
                self.draft = self.saved.clone();
                self.save_state = SaveState::Idle;
            }
            CheatSourcesPageAction::Save => {
                if self.load_error.is_some() {
                    // The file did not parse. Writing the defaults over it
                    // would destroy content the user may still want to fix by
                    // hand, so this refuses instead.
                    self.save_state = SaveState::Failed(
                        "Not saving: the existing preferences file could not be read, and \
                         overwriting it would discard it."
                            .to_string(),
                    );
                    return;
                }
                match save_cheat_sources_config_to(&self.config_path, &self.draft.to_config()) {
                    Ok(()) => {
                        self.saved = self.draft.clone();
                        self.save_state = SaveState::Saved;
                    }
                    Err(error) => self.save_state = SaveState::Failed(error.to_string()),
                }
            }
        }
    }

    /// Builds the view model. Pure: no I/O, no clock, no ordering surprises.
    pub(crate) fn view(&self) -> CheatSourcesPageView {
        let ordered = self.draft.sorted_all();

        // Consulted position is over enabled sources only, in the same order,
        // so the number matches what resolution actually does.
        let mut position = 0usize;
        let mut rows = Vec::with_capacity(ordered.len());
        for entry in &ordered {
            let consulted_position = if entry.enabled {
                position += 1;
                Some(position)
            } else {
                None
            };
            rows.push(self.row_view(entry, consulted_position));
        }

        CheatSourcesPageView {
            unresolved: self
                .draft
                .unresolved_preferences()
                .iter()
                .map(unresolved_row)
                .collect(),
            dirty: self.is_dirty(),
            config_path: self.config_path.clone(),
            save_state: self.save_state.clone(),
            load_error: self.load_error.clone(),
            pending_consequences: self.pending_consequences(&rows),
            rows,
        }
    }

    fn row_view(
        &self,
        entry: &CheatSourceEntry,
        consulted_position: Option<usize>,
    ) -> CheatSourceRowView {
        let saved_entry = self.saved.get(&entry.spec.id);
        let changed = saved_entry
            .map(|saved| saved.enabled != entry.enabled || saved.priority != entry.priority)
            .unwrap_or(false)
            || self.platform_participation_changed(&entry.spec.id);

        CheatSourceRowView {
            id: entry.spec.id.clone(),
            display_name: entry.spec.display_name.clone(),
            emulator: entry.spec.emulator.clone(),
            provider_kind: provider_kind_label(entry),
            platform_coverage: match entry.spec.platform_coverage() {
                Some(platforms) => platforms.join(", "),
                None => "All platforms".to_string(),
            },
            enabled: entry.enabled,
            priority: entry.priority,
            consulted_position,
            trust_label: BUILT_IN_INTEGRATION_LABEL,
            description: entry.spec.description.clone(),
            platforms: self.platform_views(entry),
            changed,
        }
    }

    /// The platforms a source offers a participation toggle for.
    ///
    /// A platform-specific source offers its own platforms. A source with no
    /// platform list contributes everywhere, and enumerating "everywhere"
    /// would be an unbounded and mostly meaningless list - so it offers a
    /// toggle only for platforms that already appear in the user's overrides,
    /// which are exactly the ones they have expressed an opinion about.
    fn platform_views(&self, entry: &CheatSourceEntry) -> Vec<PlatformParticipationView> {
        let mut platforms: Vec<String> = entry.spec.platforms.clone();
        for block in self.draft.platform_overrides() {
            let names_this_source = block
                .disabled_providers
                .iter()
                .flatten()
                .any(|id| id == &entry.spec.id);
            if names_this_source && !platforms.iter().any(|p| p == &block.platform) {
                platforms.push(block.platform.clone());
            }
        }

        platforms
            .into_iter()
            .map(|platform| {
                let participation = self.draft.platform_participation(&entry.spec.id, &platform);
                PlatformParticipationView {
                    platform,
                    participating: participation.participating,
                    overridden_by_source_level: participation.overridden_by_source_level,
                }
            })
            .collect()
    }

    fn platform_participation_changed(&self, id: &str) -> bool {
        let platform_names = |registry: &CheatSourceRegistry| -> Vec<(String, bool)> {
            registry
                .platform_overrides()
                .iter()
                .map(|block| {
                    (
                        block.platform.clone(),
                        block
                            .disabled_providers
                            .iter()
                            .flatten()
                            .any(|entry_id| entry_id == id),
                    )
                })
                .filter(|(_, names_it)| *names_it)
                .collect()
        };
        platform_names(&self.draft) != platform_names(&self.saved)
    }

    /// Plain-language description of what saving would do, one line per
    /// change. Empty when there is nothing pending.
    fn pending_consequences(&self, rows: &[CheatSourceRowView]) -> Vec<String> {
        if !self.is_dirty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for row in rows.iter().filter(|row| row.changed) {
            let saved = self.saved.get(&row.id);
            if let Some(saved) = saved {
                if saved.enabled != row.enabled {
                    out.push(if row.enabled {
                        format!(
                            "'{}' will be used again when looking for cheats.",
                            row.display_name
                        )
                    } else {
                        format!(
                            "'{}' will no longer be consulted. Its cached data is kept.",
                            row.display_name
                        )
                    });
                }
                if saved.priority != row.priority {
                    out.push(format!(
                        "'{}' moves to priority {} (lower is consulted first).",
                        row.display_name, row.priority
                    ));
                }
            }
            for platform in row.platforms.iter().filter(|p| !p.participating) {
                out.push(format!(
                    "'{}' will not be used for {} games, but stays enabled elsewhere.",
                    row.display_name, platform.platform
                ));
            }
        }
        if out.is_empty() {
            out.push("Preferences will be rewritten with your changes.".to_string());
        }
        out
    }
}

fn unresolved_row(entry: &UnresolvedPreference) -> UnresolvedRowView {
    UnresolvedRowView {
        detail: entry.detail.clone(),
        explanation: entry.describe(),
    }
}

/// Matches the CLI's accepted range exactly; the two must not drift.
pub(crate) const MIN_PRIORITY: u32 = 1;
pub(crate) const MAX_PRIORITY: u32 = 999;

/// Draws the page and returns at most one requested edit.
pub(crate) fn show_cheat_sources_page(
    ui: &mut egui::Ui,
    view: &CheatSourcesPageView,
    priority_drafts: &mut std::collections::HashMap<String, String>,
) -> Option<CheatSourcesPageAction> {
    let mut action = None;

    widgets::page_header(
        ui,
        "Cheat sources",
        "Which cheat catalogues ArchiveFS consults, in which order, and for which platforms.",
    );

    if let Some(error) = &view.load_error {
        widgets::banner(
            ui,
            "Preferences not read",
            &format!(
                "{error}\nShowing built-in defaults. Saving is disabled so the existing file is \
                 not overwritten."
            ),
            widgets::StatusTone::Blocked,
        );
        ui.add_space(8.0);
    }

    widgets::banner(
        ui,
        "About these sources",
        UPSTREAM_CONTENT_CAVEAT,
        widgets::StatusTone::Info,
    );
    ui.add_space(6.0);
    ui.label(egui::RichText::new(ORDERING_EXPLANATION).color(theme::muted(ui)));
    ui.add_space(10.0);

    if let Some(bar_action) = show_save_bar(ui, view) {
        action = Some(bar_action);
    }
    ui.add_space(10.0);

    for row in &view.rows {
        if action.is_none()
            && let Some(row_action) = show_source_row(ui, row, priority_drafts)
        {
            action = Some(row_action);
        }
        ui.add_space(8.0);
    }

    if !view.unresolved.is_empty() {
        ui.add_space(6.0);
        show_unresolved_section(ui, &view.unresolved);
    }

    action
}

/// The save/revert bar, plus the unsaved-change state and its consequences.
fn show_save_bar(ui: &mut egui::Ui, view: &CheatSourcesPageView) -> Option<CheatSourcesPageAction> {
    let mut action = None;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            if view.dirty {
                widgets::status_badge(ui, "Unsaved changes", widgets::StatusTone::Warning);
            } else {
                widgets::status_badge(ui, "No unsaved changes", widgets::StatusTone::Success);
            }
            ui.add_space(8.0);
            let savable = view.dirty && view.load_error.is_none();
            if widgets::action_button(
                ui,
                "Save preferences",
                widgets::ActionStyle::Primary,
                savable,
            )
            .clicked()
            {
                action = Some(CheatSourcesPageAction::Save);
            }
            if widgets::action_button(
                ui,
                "Discard changes",
                widgets::ActionStyle::Secondary,
                view.dirty,
            )
            .clicked()
            {
                action = Some(CheatSourcesPageAction::Revert);
            }
        });

        if view.dirty {
            ui.add_space(6.0);
            ui.label("Saving will:");
            for line in &view.pending_consequences {
                ui.label(format!("  • {line}"));
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Nothing is written until you save.").color(theme::muted(ui)),
            );
        }

        match &view.save_state {
            SaveState::Idle => {}
            SaveState::Saved => {
                ui.add_space(6.0);
                widgets::status_badge(ui, "Preferences saved", widgets::StatusTone::Success);
            }
            SaveState::Failed(message) => {
                ui.add_space(6.0);
                widgets::banner(ui, "Save failed", message, widgets::StatusTone::Blocked);
            }
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("File: {}", view.config_path.display()))
                .color(theme::muted(ui))
                .small(),
        );
    });
    action
}

fn show_source_row(
    ui: &mut egui::Ui,
    row: &CheatSourceRowView,
    priority_drafts: &mut std::collections::HashMap<String, String>,
) -> Option<CheatSourcesPageAction> {
    let mut action = None;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            let mut enabled = row.enabled;
            if ui.checkbox(&mut enabled, "").changed() {
                action = Some(CheatSourcesPageAction::SetEnabled {
                    id: row.id.clone(),
                    enabled,
                });
            }
            ui.label(egui::RichText::new(&row.display_name).strong());
            match row.consulted_position {
                Some(position) => {
                    widgets::status_badge(
                        ui,
                        format!("Consulted {}", ordinal(position)),
                        widgets::StatusTone::Active,
                    );
                }
                None => widgets::status_badge(ui, "Disabled", widgets::StatusTone::Pending),
            }
            if row.changed {
                widgets::status_badge(ui, "Changed", widgets::StatusTone::Warning);
            }
        });

        ui.label(
            egui::RichText::new(format!("ID: {}", row.id))
                .color(theme::muted(ui))
                .monospace(),
        );
        ui.label(
            egui::RichText::new(format!(
                "{} · {} · {}",
                row.emulator, row.provider_kind, row.platform_coverage
            ))
            .color(theme::muted(ui)),
        );
        ui.label(egui::RichText::new(row.trust_label).color(theme::muted(ui)));
        ui.add_space(4.0);
        ui.label(&row.description);

        ui.add_space(6.0);
        if action.is_none()
            && let Some(priority_action) = priority_editor(ui, row, priority_drafts)
        {
            action = Some(priority_action);
        }

        if !row.platforms.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Used for these platforms").strong());
            for platform in &row.platforms {
                ui.horizontal(|ui| {
                    let mut participating = platform.participating;
                    let toggle = ui.add_enabled(
                        !platform.overridden_by_source_level,
                        egui::Checkbox::new(&mut participating, &platform.platform),
                    );
                    if toggle.changed() && action.is_none() {
                        action = Some(CheatSourcesPageAction::SetPlatformParticipation {
                            id: row.id.clone(),
                            platform: platform.platform.clone(),
                            participating,
                        });
                    }
                    if platform.overridden_by_source_level {
                        ui.label(
                            egui::RichText::new("(source is disabled everywhere)")
                                .color(theme::muted(ui))
                                .small(),
                        );
                    } else if !platform.participating {
                        ui.label(
                            egui::RichText::new("not used for this platform")
                                .color(theme::muted(ui))
                                .small(),
                        );
                    }
                });
            }
        }
    });
    action
}

/// Priority entry that rejects out-of-range values instead of clamping.
///
/// The draft string is kept per source so a partially typed value is not
/// destroyed on every repaint, and so an invalid one can be shown as
/// invalid rather than silently corrected.
fn priority_editor(
    ui: &mut egui::Ui,
    row: &CheatSourceRowView,
    priority_drafts: &mut std::collections::HashMap<String, String>,
) -> Option<CheatSourcesPageAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label("Priority:");
        let draft = priority_drafts
            .entry(row.id.clone())
            .or_insert_with(|| row.priority.to_string());
        let response = ui.add(egui::TextEdit::singleline(draft).desired_width(60.0));
        let parsed = draft
            .parse::<u32>()
            .ok()
            .filter(|value| (MIN_PRIORITY..=MAX_PRIORITY).contains(value));
        if response.changed()
            && let Some(priority) = parsed
            && priority != row.priority
        {
            action = Some(CheatSourcesPageAction::SetPriority {
                id: row.id.clone(),
                priority,
            });
        }
        if parsed.is_none() {
            ui.label(
                egui::RichText::new(format!("enter {MIN_PRIORITY}-{MAX_PRIORITY}"))
                    .color(widgets::StatusTone::Blocked.color(ui))
                    .small(),
            );
        } else {
            ui.label(
                egui::RichText::new("lower is consulted first")
                    .color(theme::muted(ui))
                    .small(),
            );
        }
    });
    action
}

/// Entries this build cannot act on: shown, never hidden, never editable.
fn show_unresolved_section(ui: &mut egui::Ui, rows: &[UnresolvedRowView]) {
    widgets::section_header(
        ui,
        "Kept but not recognised",
        Some(
            "These lines in your preferences file name something this build does not know about. \
             They do nothing, and they are preserved exactly as written.",
        ),
    );
    widgets::card(ui, |ui| {
        for row in rows {
            ui.horizontal_top(|ui| {
                widgets::status_badge(ui, "Kept", widgets::StatusTone::Info);
                ui.add(egui::Label::new(&row.explanation).wrap());
            });
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Saving from this page does not remove them. Fix a typo by editing the file, or \
                 leave them for a build that understands them.",
            )
            .color(theme::muted(ui))
            .small(),
        );
    });
}

fn ordinal(position: usize) -> String {
    let suffix = match (position % 10, position % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{position}{suffix}")
}

#[cfg(test)]
mod tests;
