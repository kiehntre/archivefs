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

/// A card containing a vertical list of "label: status badge" rows - the
/// "Workflow state" shape shared identically by every Cheats & Mods
/// emulator adapter (RetroArch, PCSX2, Dolphin), each stating
/// profile/source/trust/inspection/destination/installation status the
/// same way. Introduced because those three call sites were byte-for-byte
/// identical except for their row contents.
pub(crate) fn status_rows(ui: &mut egui::Ui, rows: &[(&str, &str, StatusTone)]) {
    card(ui, |ui| {
        for (label, value, tone) in rows {
            ui.horizontal_wrapped(|ui| {
                ui.add_sized(
                    [132.0, 0.0],
                    egui::Label::new(egui::RichText::new(*label).strong()),
                );
                status_badge(ui, *value, *tone);
            });
        }
    });
}

/// A horizontal row of tab-like selectable buttons for choosing between a
/// small, fixed set of named options while keeping every option's label
/// reachable at a glance - lighter than a selector built from N stacked
/// cards (Cheats & Mods' RetroArch/PCSX2/Dolphin adapter chooser used to
/// be three separate cards, one per option). Built on the same
/// `egui::Button::selectable` primitive the primary sidebar navigation
/// already uses, so it participates in ordinary click and keyboard focus
/// behaviour identically - no new interaction model. Returns the newly
/// clicked option, if any; callers decide whether that differs from the
/// currently selected one. Written generically (not adapter-specific)
/// because its second intended consumer is the documented future
/// Library-tab IA migration (Health / Duplicates / Library Views as
/// tabs) - see docs/GUI_SIMPLIFICATION.md.
pub(crate) fn tab_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    options: &[(T, &str)],
    selected: T,
) -> Option<T> {
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        for (value, label) in options {
            let button = egui::Button::selectable(*value == selected, *label);
            if ui.add(button).clicked() {
                chosen = Some(*value);
            }
        }
    });
    chosen
}

/// The shared "status badge + action name [+ timestamp]" header line for
/// one activity/history entry - the piece that was rendered identically
/// (or near-identically) by all three activity surfaces: the bottom
/// activity bar, the full History & Logs page, and the Cheats & Mods
/// "Recent related activity" card. `timestamp`, when present, is the
/// already-formatted display string (the surfaces that can't spare the
/// width for one, like the bottom bar's collapsed rows, pass `None`).
/// Message rendering, per-row empty states, and what (if anything) sits in
/// the row's own right-aligned `trailing` area (a Copy button on the full
/// History & Logs page; nothing on the more space-constrained bottom bar
/// and Cheats & Mods mini card, which instead offer Copy via a context
/// menu) are deliberately left to each caller: those differ for real
/// space/interaction reasons, not by accident.
pub(crate) fn activity_row_header(
    ui: &mut egui::Ui,
    outcome_label: impl Into<String>,
    outcome_tone: StatusTone,
    action_label: impl Into<egui::RichText>,
    timestamp: Option<&str>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal_wrapped(|ui| {
        status_badge(ui, outcome_label, outcome_tone);
        ui.strong(action_label);
        if let Some(timestamp) = timestamp {
            ui.label(egui::RichText::new(timestamp).color(theme::muted(ui)));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
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
    banner(
        ui,
        headline,
        retained_note.unwrap_or(""),
        StatusTone::Warning,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_text_contains(output: &egui::FullOutput, needle: &str) -> bool {
        fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(text_shape) => text_shape.galley.text().contains(needle),
                egui::Shape::Vec(nested) => nested.iter().any(|s| shape_contains(s, needle)),
                _ => false,
            }
        }
        output
            .shapes
            .iter()
            .any(|clipped| shape_contains(&clipped.shape, needle))
    }

    #[test]
    fn status_strip_renders_every_item_with_its_own_label() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                status_strip(
                    ui,
                    &[
                        ("Ready with warnings", StatusTone::Warning),
                        ("Official repository", StatusTone::Info),
                    ],
                );
            });
        });
        assert!(rendered_text_contains(&output, "Ready with warnings"));
        assert!(rendered_text_contains(&output, "Official repository"));
    }

    #[test]
    fn status_rows_renders_every_label_and_value() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                status_rows(
                    ui,
                    &[
                        (
                            "Emulator profile",
                            "2 eligible profiles",
                            StatusTone::Success,
                        ),
                        ("Trust state", "Trusted", StatusTone::Success),
                        ("Destination", "/isolated/cheats", StatusTone::Pending),
                    ],
                );
            });
        });
        for expected in [
            "Emulator profile",
            "2 eligible profiles",
            "Trust state",
            "Trusted",
            "Destination",
            "/isolated/cheats",
        ] {
            assert!(
                rendered_text_contains(&output, expected),
                "status_rows did not render {expected:?}"
            );
        }
    }

    #[test]
    fn activity_row_header_shows_timestamp_only_when_provided() {
        let ctx = egui::Context::default();
        let with_timestamp = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                activity_row_header(
                    ui,
                    "Completed",
                    StatusTone::Success,
                    "Mount",
                    Some("2026-07-23 20:00 UTC"),
                    |_ui| {},
                );
            });
        });
        assert!(rendered_text_contains(&with_timestamp, "Completed"));
        assert!(rendered_text_contains(&with_timestamp, "Mount"));
        assert!(rendered_text_contains(
            &with_timestamp,
            "2026-07-23 20:00 UTC"
        ));

        let without_timestamp = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                activity_row_header(
                    ui,
                    "Completed",
                    StatusTone::Success,
                    "Mount",
                    None,
                    |_ui| {},
                );
            });
        });
        assert!(rendered_text_contains(&without_timestamp, "Completed"));
        assert!(rendered_text_contains(&without_timestamp, "Mount"));
    }

    #[test]
    fn activity_row_header_renders_its_trailing_content() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                activity_row_header(ui, "Failed", StatusTone::Blocked, "Unmount", None, |ui| {
                    ui.label("Copy");
                });
            });
        });
        assert!(rendered_text_contains(&output, "Failed"));
        assert!(rendered_text_contains(&output, "Unmount"));
        assert!(rendered_text_contains(&output, "Copy"));
    }

    #[test]
    fn technical_details_hides_its_body_until_expanded() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                technical_details(ui, "collapsed_by_default_test", |ui| {
                    ui.label("provider-id-9f31");
                });
            });
        });
        assert!(
            rendered_text_contains(&output, "Technical details"),
            "the disclosure's own label must always be visible"
        );
        assert!(
            !rendered_text_contains(&output, "provider-id-9f31"),
            "the body must stay collapsed until the user expands it"
        );
    }

    #[test]
    fn failure_summary_shows_the_headline_and_retained_note_directly_but_hides_detail() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                failure_summary(
                    ui,
                    "failure_summary_test",
                    "Cheat database update failed",
                    Some("Your existing cheat database is still active."),
                    "download_too_large: received 268435457 bytes",
                );
            });
        });
        assert!(rendered_text_contains(
            &output,
            "Cheat database update failed"
        ));
        assert!(rendered_text_contains(
            &output,
            "Your existing cheat database is still active."
        ));
        assert!(
            !rendered_text_contains(&output, "download_too_large"),
            "the full error text is preserved, but only behind Technical details"
        );
    }

    #[test]
    fn failure_summary_omits_the_disclosure_entirely_when_there_is_no_detail() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                failure_summary(ui, "no_detail_test", "Operation failed", None, "");
            });
        });
        assert!(!rendered_text_contains(&output, "Technical details"));
    }
}
