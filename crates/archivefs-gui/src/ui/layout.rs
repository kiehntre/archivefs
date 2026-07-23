use eframe::egui;

use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentWidth {
    Normal,
    Wide,
}

impl ContentWidth {
    fn maximum(self) -> f32 {
        match self {
            Self::Normal => theme::CONTENT_MAX_WIDTH,
            Self::Wide => theme::WIDE_CONTENT_MAX_WIDTH,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponsiveClass {
    Compact,
    Standard,
    Spacious,
}

pub(crate) fn responsive_class(available_width: f32) -> ResponsiveClass {
    if available_width < 760.0 {
        ResponsiveClass::Compact
    } else if available_width < 1240.0 {
        ResponsiveClass::Standard
    } else {
        ResponsiveClass::Spacious
    }
}

pub(crate) fn page<R>(
    ui: &mut egui::Ui,
    width: ContentWidth,
    scrollable: bool,
    scroll_id: impl std::hash::Hash,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    if scrollable {
        let viewport_height = ui.available_height();
        return egui::ScrollArea::vertical()
            .id_salt(("main_page_scroll", scroll_id))
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let keyboard_scroll = !ui.ctx().wants_keyboard_input();
                let (page_up, page_down, home, end) = ui.input(|input| {
                    (
                        input.key_pressed(egui::Key::PageUp),
                        input.key_pressed(egui::Key::PageDown),
                        input.key_pressed(egui::Key::Home),
                        input.key_pressed(egui::Key::End),
                    )
                });
                if keyboard_scroll {
                    let page = ui.clip_rect().height().max(1.0) * 0.9;
                    if page_up {
                        ui.scroll_with_delta(egui::vec2(0.0, page));
                    }
                    if page_down {
                        ui.scroll_with_delta(egui::vec2(0.0, -page));
                    }
                }
                let top = ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover());
                if keyboard_scroll && home {
                    top.scroll_to_me(Some(egui::Align::TOP));
                }
                let result = page_contents(ui, width, Some(viewport_height), add_contents);
                if keyboard_scroll && end {
                    ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                }
                result
            })
            .inner;
    }
    page_contents(ui, width, None, add_contents)
}

fn page_contents<R>(
    ui: &mut egui::Ui,
    width: ContentWidth,
    viewport_height: Option<f32>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let available = ui.available_width();
    let page_gutter = match responsive_class(available) {
        ResponsiveClass::Compact => 10.0,
        ResponsiveClass::Standard => theme::PAGE_GUTTER,
        ResponsiveClass::Spacious => 32.0,
    };
    let width = (available - page_gutter * 2.0)
        .max(320.0)
        .min(width.maximum());
    let gutter = ((available - width) * 0.5).max(0.0);
    let minimum_height = viewport_height.unwrap_or_else(|| ui.available_height());
    ui.horizontal_top(|ui| {
        ui.add_space(gutter);
        ui.allocate_ui_with_layout(
            egui::vec2(width, minimum_height),
            egui::Layout::top_down(egui::Align::Min),
            add_contents,
        )
        .inner
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_classes_cover_laptop_and_large_desktop_widths() {
        assert_eq!(responsive_class(640.0), ResponsiveClass::Compact);
        assert_eq!(responsive_class(900.0), ResponsiveClass::Standard);
        assert_eq!(responsive_class(1600.0), ResponsiveClass::Spacious);
    }
}
