use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use archivefs_core::{
    ArchiveKind, ArchiveRecord, ArchiveSnapshot, ArchiveStats, ArchiveStatus, Config, DoctorReport,
    DoctorStatus, MountState, load_read_only_snapshot_default, mount_one_archive_path,
    unmount_one_archive_path,
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
    records: Vec<ArchiveRecord>,
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
            records: snapshot.records,
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
    selected_archive: Option<PathBuf>,
    operation: Option<RunningOperation>,
    feedback: Option<ActionFeedback>,
    confirm_unmount: Option<PathBuf>,
}

impl ArchiveFsApp {
    fn new(context: egui::Context) -> Self {
        Self {
            state: start_load(context),
            filter: String::new(),
            filtered_rows: None,
            selected_archive: None,
            operation: None,
            feedback: None,
            confirm_unmount: None,
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

    fn start_operation(
        &mut self,
        context: egui::Context,
        action: ArchiveAction,
        archive_path: PathBuf,
    ) -> bool {
        self.start_operation_with_worker(context, action, archive_path, |action, archive_path| {
            perform_archive_action(action, &archive_path)
        })
    }

    fn start_operation_with_worker<F>(
        &mut self,
        context: egui::Context,
        action: ArchiveAction,
        archive_path: PathBuf,
        worker: F,
    ) -> bool
    where
        F: FnOnce(ArchiveAction, PathBuf) -> Result<String, String> + Send + 'static,
    {
        if self.operation.is_some() {
            self.feedback = Some(ActionFeedback {
                succeeded: false,
                message: "Another archive operation is already running.".to_string(),
            });
            return false;
        }

        let (sender, receiver) = mpsc::channel();
        self.confirm_unmount = None;
        self.feedback = None;
        self.operation = Some(RunningOperation { action, receiver });
        thread::spawn(move || {
            let result = worker(action, archive_path);
            let _ = sender.send(result);
            context.request_repaint();
        });
        true
    }

    fn poll_operation(&mut self, context: &egui::Context) {
        let result =
            self.operation
                .as_ref()
                .and_then(|operation| match operation.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "background archive operation stopped unexpectedly".to_string(),
                    )),
                });

        if let Some(result) = result {
            self.operation = None;
            match result {
                Ok(message) => {
                    self.feedback = Some(ActionFeedback {
                        succeeded: true,
                        message,
                    });
                    self.refresh(context);
                }
                Err(message) => {
                    self.feedback = Some(ActionFeedback {
                        succeeded: false,
                        message,
                    });
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveAction {
    Mount,
    Unmount,
}

struct RunningOperation {
    action: ArchiveAction,
    receiver: Receiver<Result<String, String>>,
}

struct ActionFeedback {
    succeeded: bool,
    message: String,
}

impl eframe::App for ArchiveFsApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_load();
        self.poll_operation(context);
        if matches!(self.state, LoadState::Loading(_)) || self.operation.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("ArchiveFS");
                ui.separator();
                ui.label("Library overview");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let loading = matches!(self.state, LoadState::Loading(_));
                    let busy = loading || self.operation.is_some();
                    if ui
                        .add_enabled(!busy, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.refresh(context);
                    }
                    if busy {
                        ui.spinner();
                    }
                });
            });
        });

        let mut retry = false;
        let mut requested_action = None;
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
                requested_action = show_loaded_data(
                    ui,
                    data,
                    LoadedViewState {
                        filter: &mut self.filter,
                        filtered_rows: &mut self.filtered_rows,
                        selected_archive: &mut self.selected_archive,
                        operation: self.operation.as_ref(),
                        feedback: self.feedback.as_ref(),
                        confirm_unmount: &mut self.confirm_unmount,
                    },
                )
            }
        });
        if retry {
            self.refresh(context);
        }
        if let Some((action, archive_path)) = requested_action {
            self.start_operation(context.clone(), action, archive_path);
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

fn perform_archive_action(action: ArchiveAction, archive_path: &Path) -> Result<String, String> {
    let config = Config::load_default().map_err(|error| error.to_string())?;
    let plan = match action {
        ArchiveAction::Mount => mount_one_archive_path(&config, archive_path),
        ArchiveAction::Unmount => unmount_one_archive_path(&config, archive_path),
    }
    .map_err(|error| error.to_string())?;

    Ok(match action {
        ArchiveAction::Mount => format!("Mounted at {}", plan.mount_path.display()),
        ArchiveAction::Unmount => format!("Unmounted {}", plan.mount_path.display()),
    })
}

struct LoadedViewState<'a> {
    filter: &'a mut String,
    filtered_rows: &'a mut Option<Vec<usize>>,
    selected_archive: &'a mut Option<PathBuf>,
    operation: Option<&'a RunningOperation>,
    feedback: Option<&'a ActionFeedback>,
    confirm_unmount: &'a mut Option<PathBuf>,
}

fn show_loaded_data(
    ui: &mut egui::Ui,
    data: &LoadedData,
    view_state: LoadedViewState<'_>,
) -> Option<(ArchiveAction, PathBuf)> {
    let LoadedViewState {
        filter,
        filtered_rows,
        selected_archive,
        operation,
        feedback,
        confirm_unmount,
    } = view_state;
    let mut requested_action = None;
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
    if let Some(feedback) = feedback {
        let color = if feedback.succeeded {
            egui::Color32::from_rgb(70, 170, 90)
        } else {
            ui.visuals().error_fg_color
        };
        ui.colored_label(color, &feedback.message);
    }
    if let Some(request) = show_selected_archive(
        ui,
        selected_record(&data.records, selected_archive.as_deref()),
        operation,
        confirm_unmount,
    ) {
        requested_action = Some(request);
    }

    if let Some(archive_path) = confirm_unmount.clone() {
        let actions_available = confirmation_actions_available(operation);
        egui::Window::new("Confirm unmount")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("Unmount this archive?");
                ui.label(archive_path.display().to_string());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(actions_available, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        *confirm_unmount = None;
                    }
                    if ui
                        .add_enabled(actions_available, egui::Button::new("Unmount"))
                        .clicked()
                    {
                        requested_action = Some((ArchiveAction::Unmount, archive_path.clone()));
                        *confirm_unmount = None;
                    }
                });
            });
    }

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
    let selected_index = selected_record_index(&data.records, selected_archive.as_deref());
    let mut clicked_index = None;
    egui::ScrollArea::horizontal()
        .id_salt("archive_status_horizontal")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(table_width(horizontal_spacing));
            show_table_cells(ui, &COLUMN_HEADERS, row_height, true, false);
            ui.separator();

            let body_height = ui.available_height().max(row_height);
            egui::ScrollArea::vertical()
                .id_salt("archive_status_vertical")
                .max_height(body_height)
                .auto_shrink([false, false])
                .show_rows(ui, row_height, visible_count, |ui, row_range| {
                    clicked_index = show_archive_rows(
                        ui,
                        &data.rows,
                        filtered_rows.as_deref(),
                        row_range,
                        row_height,
                        selected_index,
                    );
                });
        });
    if let Some(index) = clicked_index {
        *selected_archive = Some(data.records[index].mount_plan.archive.path.clone());
    }

    requested_action
}

fn fixed_row_height(text_height: f32, interact_height: f32) -> f32 {
    text_height.max(interact_height)
}

fn table_width(horizontal_spacing: f32) -> f32 {
    COLUMN_WIDTHS.iter().sum::<f32>()
        + horizontal_spacing * (COLUMN_WIDTHS.len().saturating_sub(1) as f32)
}

fn show_table_cells(
    ui: &mut egui::Ui,
    cells: &[&str; 4],
    row_height: f32,
    strong: bool,
    selected: bool,
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        for (text, width) in cells.iter().zip(COLUMN_WIDTHS) {
            let widget_text: egui::WidgetText = if strong {
                egui::RichText::new(*text).strong().into()
            } else {
                (*text).into()
            };
            let response = if strong {
                ui.add_sized(
                    [width, row_height],
                    egui::Label::new(widget_text).truncate(),
                )
            } else {
                ui.add_sized(
                    [width, row_height],
                    egui::Button::new(widget_text)
                        .selected(selected)
                        .frame(false)
                        .truncate(),
                )
            };
            clicked |= response.on_hover_text(*text).clicked();
        }
    });
    clicked
}

fn show_archive_rows(
    ui: &mut egui::Ui,
    rows: &[ArchiveRow],
    filtered_rows: Option<&[usize]>,
    row_range: Range<usize>,
    row_height: f32,
    selected_index: Option<usize>,
) -> Option<usize> {
    let mut clicked_index = None;
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
        if show_table_cells(
            ui,
            &cells,
            row_height,
            false,
            selected_index == Some(row_index),
        ) {
            clicked_index = Some(row_index);
        }
    }
    clicked_index
}

fn selected_record<'a>(
    records: &'a [ArchiveRecord],
    selected_archive: Option<&Path>,
) -> Option<&'a ArchiveRecord> {
    selected_record_index(records, selected_archive).map(|index| &records[index])
}

fn selected_record_index(
    records: &[ArchiveRecord],
    selected_archive: Option<&Path>,
) -> Option<usize> {
    let selected_archive = selected_archive?;
    records
        .iter()
        .position(|record| record.mount_plan.archive.path == selected_archive)
}

fn available_action(mount_state: MountState) -> ArchiveAction {
    match mount_state {
        MountState::Mounted => ArchiveAction::Unmount,
        MountState::Pending | MountState::MountPathExists => ArchiveAction::Mount,
    }
}

fn confirmation_actions_available(operation: Option<&RunningOperation>) -> bool {
    operation.is_none()
}

fn show_selected_archive(
    ui: &mut egui::Ui,
    record: Option<&ArchiveRecord>,
    operation: Option<&RunningOperation>,
    confirm_unmount: &mut Option<PathBuf>,
) -> Option<(ArchiveAction, PathBuf)> {
    let mut request = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong("Selected archive");
        let Some(record) = record else {
            ui.label("Select an archive row to view details.");
            return;
        };

        egui::Grid::new("selected_archive_details")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                detail_row(
                    ui,
                    "Archive path",
                    &record.mount_plan.archive.path.display().to_string(),
                );
                detail_row(
                    ui,
                    "Mount path",
                    &record.mount_plan.mount_path.display().to_string(),
                );
                detail_row(
                    ui,
                    "Platform",
                    record
                        .metadata
                        .platform
                        .as_deref()
                        .or(record.identity.platform.as_deref())
                        .unwrap_or("Unknown"),
                );
                detail_row(
                    ui,
                    "Archive format",
                    archive_kind_name(record.mount_plan.archive.kind),
                );
                detail_row(ui, "Size", &format_size(record.identity.size_bytes));
                detail_row(ui, "Mount state", &record.mount_state.to_string());
                detail_row(ui, "Health", &record.health.to_string());
                optional_detail_row(ui, "Title", record.metadata.title.as_deref());
                optional_detail_row(ui, "Region", record.metadata.region.as_deref());
                optional_detail_row(ui, "Version", record.metadata.version.as_deref());
                optional_detail_row(ui, "Disc", record.metadata.disc.as_deref());
                optional_detail_row(ui, "Publisher", record.metadata.publisher.as_deref());
                optional_detail_row(ui, "Developer", record.metadata.developer.as_deref());
                optional_detail_row(ui, "Genre", record.metadata.genre.as_deref());
                optional_detail_row(ui, "Notes", record.metadata.notes.as_deref());
                optional_detail_row(ui, "Metadata source", record.metadata.source.as_deref());
                if let Some(year) = record.metadata.release_year {
                    detail_row(ui, "Release year", &year.to_string());
                }
                if let Some(languages) = &record.metadata.languages {
                    detail_row(ui, "Languages", &languages.join(", "));
                }
            });

        ui.add_space(6.0);
        let action = available_action(record.mount_state);
        let busy = operation.is_some();
        let label = match action {
            ArchiveAction::Mount => "Mount",
            ArchiveAction::Unmount => "Unmount",
        };
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                let archive_path = record.mount_plan.archive.path.clone();
                match action {
                    ArchiveAction::Mount => request = Some((action, archive_path)),
                    ArchiveAction::Unmount => *confirm_unmount = Some(archive_path),
                }
            }
            if let Some(operation) = operation {
                ui.spinner();
                ui.label(match operation.action {
                    ArchiveAction::Mount => "Mounting...",
                    ArchiveAction::Unmount => "Unmounting...",
                });
            }
        });
    });
    request
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.strong(label);
    ui.label(value);
    ui.end_row();
}

fn optional_detail_row(ui: &mut egui::Ui, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        detail_row(ui, label, value);
    }
}

fn archive_kind_name(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Zip => "ZIP",
        ArchiveKind::SevenZip => "7z",
        ArchiveKind::Rar => "RAR",
    }
}

fn format_size(size_bytes: Option<u64>) -> String {
    size_bytes
        .map(|size| format!("{size} bytes"))
        .unwrap_or_else(|| "Unknown".to_string())
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
    use archivefs_core::{Archive, ArchiveHealth, ArchiveMetadata, MountPlan};

    fn row(search_text: &str) -> ArchiveRow {
        ArchiveRow {
            archive_path: String::new(),
            mount_path: String::new(),
            platform: String::new(),
            state: String::new(),
            search_text: search_text.to_lowercase(),
        }
    }

    fn record(archive_path: &str, mount_state: MountState) -> ArchiveRecord {
        let archive = Archive::from_path(archive_path).unwrap();
        ArchiveRecord::new(
            MountPlan::new(archive, PathBuf::from("/mnt/archivefs/Test")),
            mount_state,
            ArchiveMetadata {
                title: None,
                platform: None,
                region: None,
                languages: None,
                version: None,
                disc: None,
                publisher: None,
                developer: None,
                release_year: None,
                genre: None,
                notes: None,
                source: None,
            },
            ArchiveHealth::Pending,
        )
    }

    fn app_for_operation_tests() -> ArchiveFsApp {
        ArchiveFsApp {
            state: LoadState::Error("not loaded in this test".to_string()),
            filter: String::new(),
            filtered_rows: None,
            selected_archive: None,
            operation: None,
            feedback: None,
            confirm_unmount: None,
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

    #[test]
    fn action_availability_follows_mount_state() {
        assert_eq!(available_action(MountState::Pending), ArchiveAction::Mount);
        assert_eq!(
            available_action(MountState::MountPathExists),
            ArchiveAction::Mount
        );
        assert_eq!(
            available_action(MountState::Mounted),
            ArchiveAction::Unmount
        );
    }

    #[test]
    fn selected_record_lookup_uses_the_exact_archive_path() {
        let records = vec![
            record("/roms/Alpha.zip", MountState::Pending),
            record("/roms/Beta.7z", MountState::Mounted),
        ];

        assert_eq!(
            selected_record_index(&records, Some(Path::new("/roms/Beta.7z"))),
            Some(1)
        );
        assert_eq!(
            selected_record(&records, Some(Path::new("/roms/Beta.7z")))
                .unwrap()
                .mount_state,
            MountState::Mounted
        );
        assert!(selected_record(&records, Some(Path::new("/roms/Missing.rar"))).is_none());
        assert!(selected_record(&records, None).is_none());
    }

    #[test]
    fn start_operation_rejects_a_second_operation_without_replacing_the_receiver() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Mount,
            receiver,
        });

        assert!(!app.start_operation(
            egui::Context::default(),
            ArchiveAction::Unmount,
            PathBuf::from("/roms/Beta.7z"),
        ));
        assert_eq!(app.operation.as_ref().unwrap().action, ArchiveAction::Mount);

        sender
            .send(Ok::<String, String>("original result".to_string()))
            .unwrap();
        assert_eq!(
            app.operation.as_ref().unwrap().receiver.try_recv().unwrap(),
            Ok("original result".to_string())
        );
        let feedback = app.feedback.as_ref().unwrap();
        assert!(!feedback.succeeded);
        assert!(feedback.message.contains("already running"));
    }

    #[test]
    fn starting_an_operation_clears_pending_unmount_confirmation() {
        let mut app = app_for_operation_tests();
        app.confirm_unmount = Some(PathBuf::from("/roms/Alpha.zip"));

        assert!(app.start_operation_with_worker(
            egui::Context::default(),
            ArchiveAction::Mount,
            PathBuf::from("/roms/Beta.7z"),
            |_, _| Ok("mounted".to_string()),
        ));
        assert!(app.confirm_unmount.is_none());
        assert!(app.operation.is_some());
    }

    #[test]
    fn unmount_confirmation_actions_are_unavailable_while_busy() {
        let (_sender, receiver) = mpsc::channel();
        let operation = RunningOperation {
            action: ArchiveAction::Mount,
            receiver,
        };

        assert!(confirmation_actions_available(None));
        assert!(!confirmation_actions_available(Some(&operation)));
    }
}
