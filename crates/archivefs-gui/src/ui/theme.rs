use eframe::egui;

pub(crate) const CONTENT_MAX_WIDTH: f32 = 1080.0;
pub(crate) const WIDE_CONTENT_MAX_WIDTH: f32 = 1560.0;
pub(crate) const PAGE_GUTTER: f32 = 24.0;
pub(crate) const SECTION_GAP: f32 = 20.0;

pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(74, 126, 232);
pub(crate) const ACCENT_HOVER: egui::Color32 = egui::Color32::from_rgb(91, 143, 248);
pub(crate) const SUCCESS: egui::Color32 = egui::Color32::from_rgb(70, 176, 118);
pub(crate) const WARNING: egui::Color32 = egui::Color32::from_rgb(221, 166, 62);
pub(crate) const DANGER: egui::Color32 = egui::Color32::from_rgb(214, 82, 88);
pub(crate) const INFO: egui::Color32 = egui::Color32::from_rgb(86, 154, 214);

pub(crate) fn apply(context: &egui::Context) {
    context.style_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(27.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(16.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.menu_margin = egui::Margin::same(8);
        style.visuals.selection.bg_fill = ACCENT;
        style.visuals.widgets.active.bg_fill = ACCENT;
        style.visuals.widgets.hovered.bg_fill = ACCENT_HOVER;
        style.visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(66));
        style.visuals.panel_fill = egui::Color32::from_rgb(24, 27, 33);
        style.visuals.faint_bg_color = egui::Color32::from_rgb(31, 35, 43);
        style.visuals.extreme_bg_color = egui::Color32::from_rgb(18, 21, 26);
    });
}

pub(crate) fn muted(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().weak_text_color()
}

pub(crate) fn card_fill(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().faint_bg_color
}

pub(crate) fn border(ui: &egui::Ui) -> egui::Stroke {
    egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
}
