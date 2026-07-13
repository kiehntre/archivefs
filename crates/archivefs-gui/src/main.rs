use std::ops::Range;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use archivefs_core::{
    ArchiveRecord, ArchiveSnapshot, ArchiveStats, ArchiveStatus, DoctorReport, DoctorStatus,
    load_read_only_snapshot_default,
};
use eframe::egui;

const COLUMN_WIDTHS: [f32; 4] = [120.0, 120.0, 440.0, 520.0];
const COLUMN_HEADERS: [&str; 4] = ["Platform", "State", "Archive path", "Mount path"];

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "ArchiveFS",
        options,
        Box::new(|creation_context| {
            Ok(Box::new(ArchiveFsApp::new(
                creation_context.egui_ctx.clone(),
            )))
        }),
    )
}

struct LoadedData {
    rows: Vec<ArchiveRow>,
    stats: ArchiveStats,
    doctor: DoctorReport,
}

impl LoadedData {
    fn from_snapshot(snapshot: ArchiveSnapshot) -> Self {
        let rows = snapshot
            .records
            .iter()
            .zip(&snapshot.statuses)
            .map(|(record, status)| ArchiveRow::new(record, status))
            .collect();

        Self {
            rows,
            stats: snapshot.stats,
            doctor: snapshot.doctor,
        }
    }
}

struct ArchiveRow {
    archive_path: String,
    mount_path: String,
    platform: String,
    state: String,
    search_text: String,
}

impl ArchiveRow {
    fn new(record: &ArchiveRecord, status: &ArchiveStatus) -> Self {
        let archive_path = status.archive_path.display().to_string();
        let mount_path = status.mount_path.display().to_string();
        let platform = record
            .metadata
            .platform
            .as_deref()
            .or(record.identity.platform.as_deref())
            .unwrap_or("Unknown")
            .to_string();
        let state = status.state.to_string();
        let search_text =
            format!("{archive_path}\n{mount_path}\n{platform}\n{state}").to_lowercase();

        Self {
            archive_path,
            mount_path,
            platform,
            state,
            search_text,
        }
    }

    fn matches(&self, normalized_filter: &str) -> bool {
        self.search_text.contains(normalized_filter)
    }
}

type LoadResult = Result<LoadedData, String>;

enum LoadState {
    Loading(Receiver<LoadResult>),
    Ready(Box<LoadedData>),
    Error(String),
}

struct ArchiveFsApp {
    state: LoadState,
    filter: String,
    filtered_rows: Option<Vec<usize>>,
}

impl ArchiveFsApp {
    fn new(context: egui::Context) -> Self {
        Self {
            state: start_load(context),
            filter: String::new(),
            filtered_rows: None,
        }
    }

    fn refresh(&mut self, context: &egui::Context) {
        self.state = start_load(context.clone());
    }

    fn poll_load(&mut self) {
        let result = match &self.state {
            LoadState::Loading(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(
                    "background data loader stopped unexpectedly".to_string(),
                )),
            },
            LoadState::Ready(_) | LoadState::Error(_) => None,
        };

        if let Some(result) = result {
            self.state = match result {
                Ok(data) => {
                    self.filtered_rows = matching_row_indices(&data.rows, &self.filter);
                    LoadState::Ready(Box::new(data))
                }
                Err(error) => LoadState::Error(error),
            };
        }
    }
}

impl eframe::App for ArchiveFsApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_load();
        if matches!(self.state, LoadState::Loading(_)) {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("ArchiveFS");
                ui.separator();
                ui.label("Read-only library overview");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let loading = matches!(self.state, LoadState::Loading(_));
                    if ui
                        .add_enabled(!loading, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.refresh(context);
                    }
                    if loading {
                        ui.spinner();
                    }
                });
            });
        });

        let mut retry = false;
        egui::CentralPanel::default().show(context, |ui| match &self.state {
            LoadState::Loading(_) => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.spinner();
                    ui.heading("Loading ArchiveFS data...");
                    ui.label("Scanning runs in the background.");
                });
            }
            LoadState::Error(error) => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.colored_label(ui.visuals().error_fg_color, "Could not load ArchiveFS");
                    ui.label(error);
                    ui.add_space(8.0);
                    retry = ui.button("Try again").clicked();
                });
            }
            LoadState::Ready(data) => {
                show_loaded_data(ui, data, &mut self.filter, &mut self.filtered_rows)
            }
        });
        if retry {
            self.refresh(context);
        }
    }
}

fn start_load(context: egui::Context) -> LoadState {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = load_data();
        let _ = sender.send(result);
        context.request_repaint();
    });
    LoadState::Loading(receiver)
}

fn load_data() -> LoadResult {
    load_read_only_snapshot_default()
        .map(LoadedData::from_snapshot)
        .map_err(|error| error.to_string())
}

fn show_loaded_data(
    ui: &mut egui::Ui,
    data: &LoadedData,
    filter: &mut String,
    filtered_rows: &mut Option<Vec<usize>>,
) {
    ui.horizontal_wrapped(|ui| {
        summary_value(ui, "Total archives", data.stats.total_archives);
        summary_value(ui, "Mounted", data.stats.mounted_count);
        summary_value(ui, "Pending", data.stats.pending_count);
        ui.separator();
        let (readiness, color) = if data.doctor.is_ready() {
            ("Ready", ui.visuals().selection.bg_fill)
        } else {
            ("Needs attention", ui.visuals().error_fg_color)
        };
        ui.label("Doctor:");
        ui.colored_label(color, readiness);
    });

    ui.add_space(8.0);
    egui::CollapsingHeader::new("Doctor checks")
        .default_open(!data.doctor.is_ready())
        .show(ui, |ui| {
            egui::Grid::new("doctor_checks")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Status");
                    ui.strong("Check");
                    ui.strong("Detail");
                    ui.end_row();

                    for check in &data.doctor.checks {
                        ui.colored_label(
                            doctor_status_color(ui, check.status),
                            check.status.to_string(),
                        );
                        ui.label(&check.name);
                        ui.label(&check.detail);
                        ui.end_row();
                    }
                });
        });

    ui.separator();
    let mut filter_changed = false;
    ui.horizontal(|ui| {
        ui.label("Search:");
        filter_changed |= ui
            .add(
                egui::TextEdit::singleline(filter)
                    .hint_text("archive, mount path, platform, or state")
                    .desired_width(360.0),
            )
            .changed();
        if !filter.is_empty() && ui.small_button("Clear").clicked() {
            filter.clear();
            filter_changed = true;
        }
    });
    if filter_changed {
        *filtered_rows = matching_row_indices(&data.rows, filter);
    }

    let visible_count = filtered_rows.as_ref().map_or(data.rows.len(), Vec::len);
    ui.label(format!(
        "Showing {} of {} archives",
        visible_count,
        data.rows.len()
    ));
    ui.add_space(4.0);
    let row_height = fixed_row_height(
        ui.text_style_height(&egui::TextStyle::Body),
        ui.spacing().interact_size.y,
    );
    let horizontal_spacing = ui.spacing().item_spacing.x;
    egui::ScrollArea::horizontal()
        .id_salt("archive_status_horizontal")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(table_width(horizontal_spacing));
            show_table_cells(ui, &COLUMN_HEADERS, row_height, true);
            ui.separator();

            let body_height = ui.available_height().max(row_height);
            egui::ScrollArea::vertical()
                .id_salt("archive_status_vertical")
                .max_height(body_height)
                .auto_shrink([false, false])
                .show_rows(ui, row_height, visible_count, |ui, row_range| {
                    show_archive_rows(
                        ui,
                        &data.rows,
                        filtered_rows.as_deref(),
                        row_range,
                        row_height,
                    );
                });
        });
}

fn fixed_row_height(text_height: f32, interact_height: f32) -> f32 {
    text_height.max(interact_height)
}

fn table_width(horizontal_spacing: f32) -> f32 {
    COLUMN_WIDTHS.iter().sum::<f32>()
        + horizontal_spacing * (COLUMN_WIDTHS.len().saturating_sub(1) as f32)
}

fn show_table_cells(ui: &mut egui::Ui, cells: &[&str; 4], row_height: f32, strong: bool) {
    ui.horizontal(|ui| {
        for (text, width) in cells.iter().zip(COLUMN_WIDTHS) {
            let widget_text: egui::WidgetText = if strong {
                egui::RichText::new(*text).strong().into()
            } else {
                (*text).into()
            };
            ui.add_sized(
                [width, row_height],
                egui::Label::new(widget_text).truncate(),
            )
            .on_hover_text(*text);
        }
    });
}

fn show_archive_rows(
    ui: &mut egui::Ui,
    rows: &[ArchiveRow],
    filtered_rows: Option<&[usize]>,
    row_range: Range<usize>,
    row_height: f32,
) {
    for visible_index in row_range {
        let row_index = filtered_rows
            .map(|indices| indices[visible_index])
            .unwrap_or(visible_index);
        let row = &rows[row_index];
        let cells = [
            row.platform.as_str(),
            row.state.as_str(),
            row.archive_path.as_str(),
            row.mount_path.as_str(),
        ];
        show_table_cells(ui, &cells, row_height, false);
    }
}

fn summary_value(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.group(|ui| {
        ui.vertical_centered(|ui| {
            ui.strong(value.to_string());
            ui.small(label);
        });
    });
}

fn doctor_status_color(ui: &egui::Ui, status: DoctorStatus) -> egui::Color32 {
    match status {
        DoctorStatus::Pass => egui::Color32::from_rgb(70, 170, 90),
        DoctorStatus::Warn => egui::Color32::from_rgb(220, 170, 40),
        DoctorStatus::Fail => ui.visuals().error_fg_color,
    }
}

fn matching_row_indices(rows: &[ArchiveRow], filter: &str) -> Option<Vec<usize>> {
    let normalized_filter = filter.trim().to_lowercase();
    if normalized_filter.is_empty() {
        return None;
    }

    Some(
        rows.iter()
            .enumerate()
            .filter_map(|(index, row)| row.matches(&normalized_filter).then_some(index))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(search_text: &str) -> ArchiveRow {
        ArchiveRow {
            archive_path: String::new(),
            mount_path: String::new(),
            platform: String::new(),
            state: String::new(),
            search_text: search_text.to_lowercase(),
        }
    }

    #[test]
    fn fixed_row_height_matches_the_larger_rendering_constraint() {
        assert_eq!(fixed_row_height(14.0, 18.0), 18.0);
        assert_eq!(fixed_row_height(22.0, 18.0), 22.0);
    }

    #[test]
    fn table_width_uses_all_shared_columns_and_spacing() {
        let spacing = 8.0;
        let expected = COLUMN_WIDTHS.iter().sum::<f32>() + spacing * 3.0;

        assert_eq!(COLUMN_HEADERS.len(), COLUMN_WIDTHS.len());
        assert_eq!(table_width(spacing), expected);
    }

    #[test]
    fn empty_filter_uses_all_rows_without_an_index_allocation() {
        let rows = vec![row("Halo Xbox Mounted")];

        assert_eq!(matching_row_indices(&rows, "  "), None);
    }

    #[test]
    fn filter_indices_match_each_displayed_field_case_insensitively() {
        let rows = vec![
            row("/roms/Halo.zip /mnt/archivefs/Xbox/Halo Xbox Mounted"),
            row("/roms/Ridge.7z /mnt/archivefs/PSP/Ridge PSP Pending"),
        ];

        for query in ["HALO", "archivefs/xbox", "xBoX", "mounted"] {
            assert_eq!(matching_row_indices(&rows, query), Some(vec![0]));
        }
        assert_eq!(matching_row_indices(&rows, "playstation"), Some(Vec::new()));
    }
}
