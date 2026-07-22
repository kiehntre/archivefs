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
    ui.horizontal_top(|ui| {
        ui.add_space(gutter);
        ui.allocate_ui_with_layout(
            egui::vec2(width, ui.available_height()),
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
