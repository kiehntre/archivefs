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
    /// What the platform is called on screen. Falls back to the identifier
    /// for a platform this build does not know, which must still display as
    /// something rather than vanishing.
    pub(crate) display_name: String,
    pub(crate) participating: bool,
    /// The source is off at source level, so this toggle cannot make it
    /// contribute. Shown inactive with a reason rather than hidden, so the
    /// control does not appear to have been silently ignored.
    pub(crate) overridden_by_source_level: bool,
    /// This row exists because the user added an exception, not because the
    /// source declares the platform. Only these can be removed - a declared
    /// platform is a fact about the source, not a preference.
    pub(crate) is_exception: bool,
}

/// A platform the user may add an exception for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformChoice {
    pub(crate) id: &'static str,
    pub(crate) display_name: &'static str,
}

/// How many choices the picker shows at once.
///
/// The registry is finite (74 today), so this is not protection against an
/// unbounded list - it is what keeps the picker readable and forces the
/// search box to be the way you find something, rather than scrolling.
pub(crate) const MAX_PLATFORM_CHOICES: usize = 12;

/// Canonical platforms a source could still be given an exception for.
///
/// Drawn strictly from [`archivefs_core::platform::canonical_ids`] - the
/// same registry `canonical_platform_for_alias` resolves against - so the
/// picker can only ever offer a platform the resolver will actually match.
/// Nothing here is invented, and nothing is enumerated that the registry
/// does not already define.
///
/// `existing` is excluded, which is what makes a duplicate override
/// unreachable from the GUI: a platform already carrying one for this source
/// is not offered a second time.
pub(crate) fn available_platform_choices(existing: &[String], query: &str) -> Vec<PlatformChoice> {
    let taken: Vec<&str> = existing
        .iter()
        .map(|platform| {
            archivefs_core::canonical_platform_for_alias(platform).unwrap_or(platform.as_str())
        })
        .collect();
    let needle = query.trim().to_lowercase();

    archivefs_core::platform::canonical_ids()
        .into_iter()
        .filter(|id| !taken.contains(id))
        .map(|id| PlatformChoice {
            id,
            display_name: archivefs_core::platform::display_name_for(id),
        })
        .filter(|choice| {
            needle.is_empty()
                || choice.display_name.to_lowercase().contains(&needle)
                || choice.id.to_lowercase().contains(&needle)
        })
        .take(MAX_PLATFORM_CHOICES)
        .collect()
}

/// How many canonical platforms match `query` and are not already taken.
///
/// Separate from the truncated list so the picker can say "showing 12 of 30"
/// honestly rather than implying the 12 are all there is.
pub(crate) fn available_platform_count(existing: &[String], query: &str) -> usize {
    let taken: Vec<&str> = existing
        .iter()
        .map(|platform| {
            archivefs_core::canonical_platform_for_alias(platform).unwrap_or(platform.as_str())
        })
        .collect();
    let needle = query.trim().to_lowercase();

    archivefs_core::platform::canonical_ids()
        .into_iter()
        .filter(|id| !taken.contains(id))
        .filter(|id| {
            needle.is_empty()
                || archivefs_core::platform::display_name_for(id)
                    .to_lowercase()
                    .contains(&needle)
                || id.to_lowercase().contains(&needle)
        })
        .count()
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
    /// The source contributes to every platform, so exceptions are the only
    /// way to narrow it and the picker is offered. A source that declares its
    /// platforms already shows a toggle for each, and has nothing to add.
    pub(crate) supports_platform_exceptions: bool,
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

    /// How many sources are enabled on disk, right now - what Home shows
    /// once this page has been visited this session. Reads `saved`, not
    /// `draft`: an unsaved edit should not change what Home reports.
    pub(crate) fn enabled_source_count(&self) -> usize {
        self.saved.sorted_enabled().len()
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
            supports_platform_exceptions: entry.spec.platforms.is_empty(),
            changed,
        }
    }

    /// The platforms a source offers a participation toggle for.
    ///
    /// A platform-specific source offers exactly the platforms it declares.
    /// A source with no platform list contributes everywhere; listing all 74
    /// canonical platforms as toggles would bury the handful that matter, so
    /// it lists only the exceptions that exist and offers the picker
    /// (`supports_platform_exceptions`) to add one.
    fn platform_views(&self, entry: &CheatSourceEntry) -> Vec<PlatformParticipationView> {
        let declared = &entry.spec.platforms;
        let mut platforms: Vec<String> = declared.clone();
        for block in self.draft.platform_overrides() {
            let names_this_source = block
                .disabled_providers
                .iter()
                .flatten()
                .any(|id| id == &entry.spec.id);
            // Compared canonically: a file may name one platform in several
            // blocks, and two rows for the same platform - showing the same
            // value, with two controls fighting over it - is worse than one.
            if names_this_source
                && !platforms
                    .iter()
                    .any(|seen| same_platform(seen, &block.platform))
            {
                platforms.push(block.platform.clone());
            }
        }

        platforms
            .into_iter()
            .map(|platform| {
                let participation = self.draft.platform_participation(&entry.spec.id, &platform);
                PlatformParticipationView {
                    display_name: archivefs_core::platform::display_name_for(&platform).to_string(),
                    is_exception: !declared.iter().any(|declared| declared == &platform),
                    platform,
                    participating: participation.participating,
                    overridden_by_source_level: participation.overridden_by_source_level,
                }
            })
            .collect()
    }

    /// Platforms whose participation differs between draft and disk, with the
    /// value the draft would save.
    ///
    /// Considers platforms named by *either* side, so removing an exception -
    /// which takes its block away entirely, leaving nothing in the draft to
    /// iterate - is reported just as clearly as adding one.
    fn participation_changes(&self, id: &str) -> Vec<(String, bool)> {
        let mentioned = |registry: &CheatSourceRegistry| -> Vec<String> {
            registry
                .platform_overrides()
                .iter()
                .filter(|block| {
                    block
                        .disabled_providers
                        .iter()
                        .flatten()
                        .any(|entry_id| entry_id == id)
                })
                .map(|block| block.platform.clone())
                .collect()
        };

        let mut platforms = mentioned(&self.draft);
        for platform in mentioned(&self.saved) {
            if !platforms.iter().any(|seen| same_platform(seen, &platform)) {
                platforms.push(platform);
            }
        }
        platforms.dedup_by(|a, b| same_platform(a, b));

        platforms
            .into_iter()
            .filter_map(|platform| {
                let now = self
                    .draft
                    .platform_participation(id, &platform)
                    .participating;
                let before = self
                    .saved
                    .platform_participation(id, &platform)
                    .participating;
                (now != before).then_some((platform, now))
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
            // Only participation that actually differs from disk. Listing
            // every non-participating platform announced exceptions the user
            // saved long ago as though saving would newly apply them.
            for (platform, now_participating) in self.participation_changes(&row.id) {
                let platform = archivefs_core::platform::display_name_for(&platform);
                // A source turned off outright is consulted nowhere, so a
                // per-platform exception under it changes nothing yet. Saying
                // it "stays enabled elsewhere" - or that it will be used again
                // - would promise behaviour the user will not observe.
                out.push(if !row.enabled {
                    format!(
                        "'{}' is turned off everywhere, so this {platform} setting \
                         has no effect until it is turned back on.",
                        row.display_name
                    )
                } else if now_participating {
                    format!(
                        "'{}' will be used for {platform} games again.",
                        row.display_name
                    )
                } else {
                    format!(
                        "'{}' will not be used for {platform} games, but stays enabled elsewhere.",
                        row.display_name
                    )
                });
            }
        }
        if out.is_empty() {
            out.push("Preferences will be rewritten with your changes.".to_string());
        }
        out
    }
}

/// Whether two platform strings name the same platform.
///
/// Compared canonically, so an alias and a canonical id are recognised as one
/// platform rather than counted twice. Unresolvable names fall back to an
/// exact comparison, which keeps them distinct from everything else instead
/// of silently collapsing together.
fn same_platform(left: &str, right: &str) -> bool {
    match (
        archivefs_core::canonical_platform_for_alias(left),
        archivefs_core::canonical_platform_for_alias(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
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

/// Unsubmitted UI text and which disclosure is open.
///
/// Deliberately not part of [`CheatSourcesPageState`]: none of it is policy.
/// A half-typed priority on the way from "1" to "15" must not be applied,
/// and an open picker is not a preference - neither belongs in something
/// whose difference from disk defines the unsaved-change state.
#[derive(Default)]
pub(crate) struct CheatSourcesPageUi {
    /// In-progress priority text, keyed by source ID.
    pub(crate) priority_drafts: std::collections::HashMap<String, String>,
    /// Which source's platform picker is open, if any. At most one, so the
    /// page never shows two competing searches.
    pub(crate) open_picker: Option<String>,
    /// The picker's search text.
    pub(crate) picker_query: String,
}

impl CheatSourcesPageUi {
    /// Forgets every unsubmitted edit.
    ///
    /// Called on Discard: leaving typed text behind after "Discard changes"
    /// would show a value that is no longer anywhere in the state.
    pub(crate) fn clear(&mut self) {
        self.priority_drafts.clear();
        self.open_picker = None;
        self.picker_query.clear();
    }
}

/// Draws the page and returns at most one requested edit.
pub(crate) fn show_cheat_sources_page(
    ui: &mut egui::Ui,
    view: &CheatSourcesPageView,
    ui_state: &mut CheatSourcesPageUi,
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
            && let Some(row_action) = show_source_row(ui, row, ui_state)
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
    ui_state: &mut CheatSourcesPageUi,
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
            && let Some(priority_action) = priority_editor(ui, row, &mut ui_state.priority_drafts)
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
                        egui::Checkbox::new(&mut participating, &platform.display_name),
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
                    // Only an exception can be removed. A platform the source
                    // declares is a fact about the source, not a preference,
                    // so there is nothing there to take away.
                    if platform.is_exception
                        && !platform.participating
                        && widgets::action_button(
                            ui,
                            "Remove exception",
                            widgets::ActionStyle::Quiet,
                            true,
                        )
                        .clicked()
                        && action.is_none()
                    {
                        action = Some(CheatSourcesPageAction::SetPlatformParticipation {
                            id: row.id.clone(),
                            platform: platform.platform.clone(),
                            participating: true,
                        });
                    }
                });
            }
        }

        if row.supports_platform_exceptions
            && action.is_none()
            && let Some(picker_action) = platform_exception_picker(ui, row, ui_state)
        {
            action = Some(picker_action);
        }
    });
    action
}

/// Lets a source that applies everywhere be excepted from one platform.
///
/// # Why a search box and not a list of toggles
///
/// A cross-platform source contributes to all 74 canonical platforms. Drawing
/// 74 checkboxes per source would bury the one or two a user actually cares
/// about, and drawing none - what this page did before - left no way to
/// create the first exception at all, so the feature was reachable only by
/// hand-editing the file.
///
/// Every candidate comes from the canonical registry
/// ([`available_platform_choices`]), so the picker cannot offer a platform
/// the resolver would not match, and platforms already carrying an exception
/// for this source are excluded, which is what makes a duplicate unreachable.
fn platform_exception_picker(
    ui: &mut egui::Ui,
    row: &CheatSourceRowView,
    ui_state: &mut CheatSourcesPageUi,
) -> Option<CheatSourcesPageAction> {
    let mut action = None;
    let is_open = ui_state.open_picker.as_deref() == Some(row.id.as_str());

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let label = if is_open {
            "Cancel"
        } else {
            "Don't use for a platform…"
        };
        if widgets::action_button(ui, label, widgets::ActionStyle::Secondary, true).clicked() {
            if is_open {
                ui_state.open_picker = None;
            } else {
                ui_state.open_picker = Some(row.id.clone());
            }
            ui_state.picker_query.clear();
        }
        ui.label(
            egui::RichText::new("This source is used for every platform unless excepted.")
                .color(theme::muted(ui))
                .small(),
        );
    });

    if !is_open {
        return action;
    }

    let existing: Vec<String> = row
        .platforms
        .iter()
        .map(|platform| platform.platform.clone())
        .collect();

    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Find a platform:");
            ui.add(
                egui::TextEdit::singleline(&mut ui_state.picker_query)
                    .hint_text("e.g. PlayStation 2")
                    .desired_width(220.0),
            );
        });

        let choices = available_platform_choices(&existing, &ui_state.picker_query);
        let total = available_platform_count(&existing, &ui_state.picker_query);

        if choices.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No platform matches. Platforms this source is already excepted from are not \
                     offered again.",
                )
                .color(theme::muted(ui)),
            );
            return;
        }

        for choice in &choices {
            ui.horizontal(|ui| {
                // Stated per row, because the whole point of the control is
                // to change it and the user should not have to infer it.
                ui.label(
                    egui::RichText::new("Currently used")
                        .color(widgets::StatusTone::Success.color(ui))
                        .small(),
                );
                if widgets::action_button(
                    ui,
                    format!("Stop using for {}", choice.display_name),
                    widgets::ActionStyle::Secondary,
                    true,
                )
                .clicked()
                    && action.is_none()
                {
                    action = Some(CheatSourcesPageAction::SetPlatformParticipation {
                        id: row.id.clone(),
                        platform: choice.id.to_string(),
                        participating: false,
                    });
                }
            });
        }

        if total > choices.len() {
            ui.label(
                egui::RichText::new(format!(
                    "Showing {} of {total} matches. Type to narrow the search.",
                    choices.len()
                ))
                .color(theme::muted(ui))
                .small(),
            );
        }
    });

    // Close the picker once it has done its job, so the page returns to the
    // list showing the exception that was just added.
    if action.is_some() {
        ui_state.open_picker = None;
        ui_state.picker_query.clear();
    }
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
