use std::path::{Path, PathBuf};

use eframe::egui;

use crate::{dialogs::filedialog::SnowFileDialog, settings::AppSettings};

/// Dialog to create a blank HDD image
#[derive(Default)]
pub struct DiskImageDialog {
    open: bool,
    current_fn: String,
    current_size: f64,
    bus: String,
    slot: String,
    /// Where the new disk goes, for the text of the dialog: "SCSI ID #3"
    place: String,

    browse_dialog: SnowFileDialog,
    result: Option<DiskImageDialogResult>,
}

pub struct DiskImageDialogResult {
    pub filename: PathBuf,
    pub size: usize,
    /// The bus and the slot the new disk goes in
    pub bus: String,
    pub slot: String,
}

impl DiskImageDialog {
    pub fn update(&mut self, ctx: &egui::Context, frame: &eframe::Frame, settings: &AppSettings) {
        if !self.open {
            return;
        }

        self.browse_dialog.update(ctx, frame);
        if *self.browse_dialog.state() == egui_file_dialog::DialogState::Open {
            return;
        }
        if let Some(path) = self.browse_dialog.take_picked() {
            self.current_fn = path.to_string_lossy().to_string();
        }

        egui::Modal::new(egui::Id::new("Create disk image")).show(ctx, |ui| {
            ui.set_width(250.0);

            ui.heading("Create disk image");

            egui::Grid::new("create_disk_dialog").show(ui, |ui| {
                ui.label("Filename:");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.current_fn);
                    if ui.button("Browse").clicked() {
                        self.browse_dialog.save_file(settings.native_file_dialogs);
                    }
                });
                ui.end_row();

                ui.label("Size (MB):");
                ui.add(egui::Slider::new(&mut self.current_size, 1.0..=1024.0).step_by(0.5));
                ui.end_row();
            });

            ui.separator();
            ui.label(format!(
                "{} The new disk will be attached at {}.",
                egui_material_icons::icons::ICON_INFO,
                self.place
            ));
            ui.label("Machine should be reset after attaching new drives!");
            ui.separator();

            egui::Sides::new().show(
                ui,
                |_ui| {},
                |ui| {
                    if ui.button("Create").clicked() {
                        let size = (self.current_size * 1024.0) as usize * 1024;
                        assert_eq!(size % 512, 0);
                        self.result = Some(DiskImageDialogResult {
                            filename: PathBuf::from(&self.current_fn),
                            size,
                            bus: self.bus.clone(),
                            slot: self.slot.clone(),
                        });
                        self.open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.open = false;
                    }
                },
            );
        });
    }

    pub fn open(&mut self, bus: &str, slot: &str, place: String, initial_path: &Path) {
        let filename = format!("hdd{}.img", slot);
        let full_path = initial_path.join(&filename);
        self.bus = bus.to_string();
        self.slot = slot.to_string();
        self.place = place;
        self.open = true;
        self.current_fn = full_path.to_string_lossy().into();
        self.current_size = 20.0;
        self.browse_dialog.config_mut().initial_directory = initial_path.to_path_buf();
        self.browse_dialog.config_mut().default_file_name = filename;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_result(&mut self) -> Option<DiskImageDialogResult> {
        self.result.take()
    }
}
