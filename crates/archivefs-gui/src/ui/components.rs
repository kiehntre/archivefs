use std::path::Path;

use eframe::egui;

use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionStyle {
    Primary,
    Secondary,
    Quiet,
    Destructive,
}

pub(crate) fn action_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    style: ActionStyle,
    enabled: bool,
) -> egui::Response {
    let button = match style {
        ActionStyle::Primary => egui::Button::new(label).fill(theme::ACCENT),
        ActionStyle::Secondary => egui::Button::new(label),
        ActionStyle::Quiet => egui::Button::new(label).frame(false),
        ActionStyle::Destructive => egui::Button::new(label).fill(theme::DANGER),
    };
    ui.add_enabled(enabled, button)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusTone {
    Success,
    Warning,
    Blocked,
    Pending,
    Active,
    Info,
}

impl StatusTone {
    pub(crate) fn color(self, ui: &egui::Ui) -> egui::Color32 {
        match self {
            Self::Success => theme::SUCCESS,
            Self::Warning => theme::WARNING,
            Self::Blocked => theme::DANGER,
            Self::Pending => theme::muted(ui),
            Self::Active => theme::ACCENT_HOVER,
            Self::Info => theme::INFO,
        }
    }
}

pub(crate) fn status_badge(ui: &mut egui::Ui, label: impl Into<String>, tone: StatusTone) {
    let color = tone.color(ui);
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.7)))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label.into()).color(color).strong());
        });
}

pub(crate) fn page_header(ui: &mut egui::Ui, title: &str, purpose: &str) {
    ui.heading(title);
    ui.label(egui::RichText::new(purpose).color(theme::muted(ui)));
    ui.add_space(theme::SECTION_GAP);
}

pub(crate) fn section_header(ui: &mut egui::Ui, title: &str, description: Option<&str>) {
    ui.label(egui::RichText::new(title).size(19.0).strong());
    if let Some(description) = description {
        ui.label(egui::RichText::new(description).color(theme::muted(ui)));
    }
    ui.add_space(4.0);
}

pub(crate) fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::card_fill(ui))
        .stroke(theme::border(ui))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(14))
        .show(ui, add_contents)
        .inner
}

pub(crate) fn empty_state(
    ui: &mut egui::Ui,
    title: &str,
    detail: &str,
    action_label: Option<&str>,
) -> bool {
    card(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(title).size(18.0).strong());
            ui.label(egui::RichText::new(detail).color(theme::muted(ui)));
            let clicked = action_label.is_some_and(|label| {
                ui.add_space(6.0);
                action_button(ui, label, ActionStyle::Primary, true).clicked()
            });
            ui.add_space(12.0);
            clicked
        })
        .inner
    })
}

pub(crate) fn banner(ui: &mut egui::Ui, title: &str, detail: &str, tone: StatusTone) {
    let color = tone.color(ui);
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.65)))
        .corner_radius(7)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                status_badge(ui, title, tone);
                ui.add(egui::Label::new(detail).wrap());
            });
        });
}

pub(crate) fn path_value(ui: &mut egui::Ui, label: &str, path: &Path) -> bool {
    copyable_value(ui, label, &path.display().to_string())
}

/// The one place every "Technical details" / "Open details" disclosure in
/// the app should go through, so provider IDs, digests, manifest paths,
/// hashes, and other internals are always tucked behind the same label in
/// the same collapsed-by-default shape instead of each call site inventing
/// its own `CollapsingHeader` title and default state.
pub(crate) fn technical_details<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    egui::CollapsingHeader::new("Technical details")
        .id_salt(id_salt)
        .default_open(false)
        .show(ui, add_contents)
        .body_returned
}

/// A compact single row of status badges, for pages that used to stack
/// several large cards each stating one piece of status (profile, source,
/// trust, identity, ...). Wraps onto more than one line if the available
/// width is too narrow for all of them.
pub(crate) fn status_strip(ui: &mut egui::Ui, items: &[(&str, StatusTone)]) {
    ui.horizontal_wrapped(|ui| {
        for (label, tone) in items {
            status_badge(ui, *label, *tone);
        }
    });
}

/// One consistent presentation for "an operation failed, but the previous
/// good result is still active" - the shape most retrieval/refresh
/// failures in ArchiveFS take (the old cheat database, the old catalogue,
/// the old snapshot all remain usable). Shows the plain-language headline
/// and, when the prior state is still active, a short retained-state note,
/// directly; the original detailed error text is preserved in full but
/// moved behind [`technical_details`] rather than duplicated across a page
/// alert, an activity-bar entry, and an activity-panel entry.
pub(crate) fn failure_summary(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    headline: &str,
    retained_note: Option<&str>,
    detail: &str,
) {
    banner(ui, headline, retained_note.unwrap_or(""), StatusTone::Warning);
    if !detail.is_empty() {
        technical_details(ui, id_salt, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(detail).monospace()).wrap());
        });
    }
}

pub(crate) fn copyable_value(ui: &mut egui::Ui, label: &str, full: &str) -> bool {
    let mut copy = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).strong());
        let available = (ui.available_width() - 54.0).max(120.0);
        ui.add_sized(
            [available, ui.spacing().interact_size.y],
            egui::Label::new(egui::RichText::new(full).monospace()).truncate(),
        )
        .on_hover_text(full);
        copy = action_button(ui, "Copy", ActionStyle::Quiet, true).clicked();
    });
    copy
}
