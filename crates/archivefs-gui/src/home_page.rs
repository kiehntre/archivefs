//! The Home page: "What would you like to do?"
//!
//! # Why this page exists
//!
//! Every one of the seven workflows here already shipped on its own page.
//! What was missing was a single place that names them as tasks, in plain
//! language, and says honestly whether each one looks ready, not yet set
//! up, or currently unavailable. Nothing on this page is a new capability -
//! it is a front door onto capabilities that already exist.
//!
//! # The view model, and why some cards say "not checked yet"
//!
//! Following `romm_source`/`cheat_sources_page`, authoritative state is
//! turned into a [`HomeView`] by the pure [`build_home_view`], and the
//! drawing code only draws. `main.rs` builds [`HomeInputs`] from whatever
//! is already loaded on `ArchiveFsApp` - never from a fresh read.
//!
//! Three of the seven cards (Cheats & Mods' source count, DAT Sources'
//! registry count, RomM's provider state) read state that ArchiveFS
//! deliberately does not load until the user visits that page - loading it
//! eagerly here would mean opening Home always triggers three background
//! reads nobody asked for, network-shaped or not. So when the real page
//! has not been visited yet this session, [`CardReadiness::Unknown`] is
//! shown - "open it to check" - rather than guessing, or lying that
//! nothing is configured.

use crate::ui::{components as widgets, theme};
use archivefs_core::{SetupDiagnostic, SetupDiagnosticStatus};
use eframe::egui;

/// One of the seven task-oriented destinations Home can send a user to.
/// `main.rs` maps each variant to the `MainView` (and, for the two that
/// need it, the extra dispatch logic) its sidebar button already uses -
/// Home never invents a second way to reach a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HomeCard {
    BuildLibrary,
    BrowseGames,
    CheatsAndMods,
    CheatSources,
    DatSources,
    RomM,
    CheckSetup,
    Settings,
}

/// Honest readiness for a card, distinguishing four situations that read
/// very differently to a user:
///
/// - [`Self::NotConfigured`]: nothing is set up yet. Expected on a fresh
///   install, never shown as a fault.
/// - [`Self::Unavailable`]: it is configured, but something about it is
///   currently not working (a real error/warning, or a provider state that
///   is not ready). Never conflated with "not configured".
/// - [`Self::Ready`]: configured and, as far as already-loaded state shows,
///   usable.
/// - [`Self::Unknown`]: Home has not loaded enough to say. Only used for
///   the three lazily-loaded destinations described in the module docs,
///   and only before their real page has been visited this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CardReadiness {
    NotConfigured(String),
    Unavailable(String),
    Ready(String),
    Unknown(String),
}

impl CardReadiness {
    fn tone(&self) -> widgets::StatusTone {
        match self {
            Self::NotConfigured(_) => widgets::StatusTone::Pending,
            Self::Unavailable(_) => widgets::StatusTone::Warning,
            Self::Ready(_) => widgets::StatusTone::Success,
            Self::Unknown(_) => widgets::StatusTone::Info,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::NotConfigured(text)
            | Self::Unavailable(text)
            | Self::Ready(text)
            | Self::Unknown(text) => text,
        }
    }
}

/// One rendered card: what it is, why it matters, whether it looks ready,
/// and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeCardView {
    pub(crate) card: HomeCard,
    pub(crate) title: &'static str,
    pub(crate) explanation: &'static str,
    /// `None` for the one card (Settings) with no single configured/not
    /// configured state to report.
    pub(crate) readiness: Option<CardReadiness>,
    pub(crate) action_label: &'static str,
    /// A second, smaller link some cards offer alongside their primary
    /// action - e.g. Cheats & Mods also links to Cheat Sources.
    pub(crate) secondary: Option<(HomeCard, &'static str)>,
}

/// Whether, and how, to show the banner above the card grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeBanner {
    /// No configuration file has ever been seen this session: a genuine
    /// first run, not a fault.
    FreshInstall,
    /// A configuration file was seen and confirmed earlier this session,
    /// and is no longer found. Never shown with [`Self::FreshInstall`]'s
    /// cheerful wording - see `missing_config_is_first_run` in `main.rs`,
    /// which this page reuses rather than re-deriving the distinction.
    ConfigDisappeared,
    /// Nothing to say: either configured, or a check is still loading.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeView {
    pub(crate) banner: HomeBanner,
    pub(crate) cards: Vec<HomeCardView>,
}

/// Everything [`build_home_view`] needs, gathered by `main.rs` from state
/// that is already loaded - building this never reads a file, starts a
/// thread, or makes a request.
pub(crate) struct HomeInputs<'a> {
    pub(crate) source_folder_count: usize,
    pub(crate) has_database: bool,
    /// `None` while diagnostics are still loading in the background, or if
    /// the last diagnostics run itself failed.
    pub(crate) diagnostics: Option<&'a [SetupDiagnostic]>,
    pub(crate) config_missing: bool,
    /// Mirrors `missing_config_is_first_run(config_previously_confirmed)`.
    pub(crate) first_run: bool,
    /// `None` until the Cheat Sources page has been visited this session.
    pub(crate) cheat_sources_enabled_count: Option<usize>,
    /// `None` until the DAT Sources page has been visited this session.
    pub(crate) dat_sources_registered_count: Option<usize>,
    /// `None` until the Sources page has loaded RomM status this session.
    /// The label is already plain language (`ProviderState::label`).
    pub(crate) romm_state_label: Option<RommReadinessLabel>,
}

/// The three buckets a `ProviderState` collapses into for Home, plus its
/// existing display label - built by `main.rs` so this module does not
/// need to depend on `archivefs_core::identity_source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RommReadinessLabel {
    NotConfigured(&'static str),
    Unavailable(&'static str),
    Ready(&'static str),
}

const READ_ONLY_DAT_NOTE: &str = "Registering and auditing DAT files is always read-only towards your ROMs: nothing is renamed, moved, or rewritten.";
const READ_ONLY_ROMM_NOTE: &str = "ArchiveFS treats RomM as a read-only identity source: nothing in your RomM library is ever changed.";

/// Turns already-loaded state into what Home draws. Pure: the same inputs
/// always produce the same view, and nothing here touches disk or a
/// socket.
pub(crate) fn build_home_view(inputs: &HomeInputs) -> HomeView {
    let banner = if inputs.config_missing {
        if inputs.first_run {
            HomeBanner::FreshInstall
        } else {
            HomeBanner::ConfigDisappeared
        }
    } else {
        HomeBanner::None
    };

    let library_readiness = if inputs.source_folder_count == 0 {
        CardReadiness::NotConfigured("No source folders yet".to_string())
    } else {
        CardReadiness::Ready(format!(
            "{} source folder{} configured",
            inputs.source_folder_count,
            if inputs.source_folder_count == 1 {
                ""
            } else {
                "s"
            }
        ))
    };

    let browse_readiness = if inputs.source_folder_count == 0 {
        CardReadiness::NotConfigured("Add a source folder first".to_string())
    } else if !inputs.has_database {
        CardReadiness::Unknown("Not scanned yet this session".to_string())
    } else {
        CardReadiness::Ready("Ready to browse".to_string())
    };

    let cheats_readiness = match inputs.cheat_sources_enabled_count {
        None => CardReadiness::Unknown("Open Cheat Sources to check status".to_string()),
        Some(0) => CardReadiness::NotConfigured("No cheat sources enabled".to_string()),
        Some(n) => CardReadiness::Ready(format!("{n} cheat source{} enabled", plural(n))),
    };

    let dat_readiness = match inputs.dat_sources_registered_count {
        None => CardReadiness::Unknown("Open DAT Sources to check status".to_string()),
        Some(0) => CardReadiness::NotConfigured("No DAT sources registered yet".to_string()),
        Some(n) => CardReadiness::Ready(format!("{n} DAT source{} registered", plural(n))),
    };

    let romm_readiness = match &inputs.romm_state_label {
        None => CardReadiness::Unknown("Open Sources to check status".to_string()),
        Some(RommReadinessLabel::NotConfigured(label)) => {
            CardReadiness::NotConfigured((*label).to_string())
        }
        Some(RommReadinessLabel::Unavailable(label)) => {
            CardReadiness::Unavailable((*label).to_string())
        }
        Some(RommReadinessLabel::Ready(label)) => CardReadiness::Ready((*label).to_string()),
    };

    let setup_readiness = summarize_setup_checks(inputs.diagnostics, inputs.config_missing);

    let cards = vec![
        HomeCardView {
            card: HomeCard::BuildLibrary,
            title: "Build my library",
            explanation: "ArchiveFS needs one or more source folders before it can scan for archives.",
            readiness: Some(library_readiness),
            action_label: "Open Sources",
            secondary: None,
        },
        HomeCardView {
            card: HomeCard::BrowseGames,
            title: "Browse my games",
            explanation: "See the archives ArchiveFS has found so far, organized and searchable.",
            readiness: Some(browse_readiness),
            action_label: "Open Library",
            secondary: None,
        },
        HomeCardView {
            card: HomeCard::CheatsAndMods,
            title: "Add or manage cheats",
            explanation: "Install trusted cheats and patches for an archive you select. Not every game or every source has cheats available.",
            readiness: Some(cheats_readiness),
            action_label: "Open Cheats & Mods",
            secondary: Some((HomeCard::CheatSources, "Manage Cheat Sources")),
        },
        HomeCardView {
            card: HomeCard::DatSources,
            title: "Register DAT files",
            explanation: READ_ONLY_DAT_NOTE,
            readiness: Some(dat_readiness),
            action_label: "Open DAT Sources",
            secondary: None,
        },
        HomeCardView {
            card: HomeCard::RomM,
            title: "Connect RomM",
            explanation: READ_ONLY_ROMM_NOTE,
            readiness: Some(romm_readiness),
            action_label: "Open Sources",
            secondary: None,
        },
        HomeCardView {
            card: HomeCard::CheckSetup,
            title: "Check my setup",
            explanation: "A summary of ArchiveFS's own configuration and environment checks. Opens Doctor for details - nothing here runs a repair.",
            readiness: Some(setup_readiness),
            action_label: "Open Doctor",
            secondary: None,
        },
        HomeCardView {
            card: HomeCard::Settings,
            title: "Settings",
            explanation: "Mount, emulator profile, and other ArchiveFS preferences.",
            readiness: None,
            action_label: "Open Settings",
            secondary: None,
        },
    ];

    HomeView { banner, cards }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Summarizes `SetupDiagnostics.checks` into one readiness for the "Check
/// my setup" card. Reads only what diagnostics already computed - never
/// starts a Doctor scan, which requires its own explicit action.
fn summarize_setup_checks(
    checks: Option<&[SetupDiagnostic]>,
    config_missing: bool,
) -> CardReadiness {
    let Some(checks) = checks else {
        return CardReadiness::Unknown("Checking...".to_string());
    };
    let errors = checks
        .iter()
        .filter(|c| c.status == SetupDiagnosticStatus::Error)
        .count();
    let warnings = checks
        .iter()
        .filter(|c| c.status == SetupDiagnosticStatus::Warning)
        .count();
    if errors > 0 {
        let verb = if errors == 1 { "needs" } else { "need" };
        CardReadiness::Unavailable(format!("{errors} check{} {verb} attention", plural(errors)))
    } else if warnings > 0 {
        CardReadiness::Unavailable(format!("{warnings} warning{}", plural(warnings)))
    } else if config_missing {
        CardReadiness::NotConfigured("Not configured yet - expected on a fresh install".to_string())
    } else {
        CardReadiness::Ready("All checks passed".to_string())
    }
}

/// Draws Home and reports which card (primary or secondary action) was
/// clicked, if any. Draws only what `view` says - no state, no I/O.
pub(crate) fn show_home_page(ui: &mut egui::Ui, view: &HomeView) -> Option<HomeCard> {
    let mut clicked = None;

    widgets::page_header(ui, "Home", "What would you like to do?");

    match view.banner {
        HomeBanner::FreshInstall => {
            widgets::banner(
                ui,
                "Welcome to ArchiveFS",
                "ArchiveFS is not configured yet - that is expected on a fresh install, not an \
                 error. Pick a task below to get started.",
                widgets::StatusTone::Info,
            );
            ui.add_space(theme::SECTION_GAP);
        }
        HomeBanner::ConfigDisappeared => {
            widgets::banner(
                ui,
                "Configuration file is no longer found",
                "ArchiveFS found your configuration earlier in this session, and it is no \
                 longer present. Check Doctor before starting a new task.",
                widgets::StatusTone::Warning,
            );
            ui.add_space(theme::SECTION_GAP);
        }
        HomeBanner::None => {}
    }

    ui.vertical(|ui| {
        for card in &view.cards {
            widgets::card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(card.title).size(17.0).strong());
                    if let Some(readiness) = &card.readiness {
                        widgets::status_badge(ui, readiness.label(), readiness.tone());
                    }
                });
                ui.label(egui::RichText::new(card.explanation).color(theme::muted(ui)));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if widgets::action_button(
                        ui,
                        card.action_label,
                        widgets::ActionStyle::Primary,
                        true,
                    )
                    .clicked()
                    {
                        clicked = Some(card.card);
                    }
                    if let Some((secondary_card, secondary_label)) = card.secondary
                        && widgets::action_button(
                            ui,
                            secondary_label,
                            widgets::ActionStyle::Secondary,
                            true,
                        )
                        .clicked()
                    {
                        clicked = Some(secondary_card);
                    }
                });
            });
            ui.add_space(theme::SECTION_GAP / 2.0);
        }
    });

    clicked
}

#[cfg(test)]
mod tests;
