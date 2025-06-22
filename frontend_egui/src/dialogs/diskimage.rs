use std::path::{Path, PathBuf};

use eframe::egui;
use egui_file_dialog::FileDialog;

/// Dialog to create a blank HDD image
#[derive(Default)]
pub struct DiskImageDialog {
    open: bool,
    current_fn: String,
    current_size: f64,
    scsi_id: usize,

    browse_dialog: FileDialog,
    result: Option<DiskImageDialogResult>,
}

pub struct DiskImageDialogResult {
    pub filename: PathBuf,
    pub size: usize,
    pub scsi_id: usize,
}

impl DiskImageDialog {
    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        self.browse_dialog.update(ctx);
        if self.browse_dialog.state() == egui_file_dialog::DialogState::Open {
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
                        self.browse_dialog.save_file();
                    }
                });
                ui.end_row();

                ui.label("Size (MB):");
                ui.add(egui::Slider::new(&mut self.current_size, 1.0..=1024.0).step_by(0.5));
                ui.end_row();
            });

            ui.separator();
            ui.label(format!(
                "{} The new disk will be attached at SCSI ID #{}.",
                egui_material_icons::icons::ICON_INFO,
                self.scsi_id
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
                            scsi_id: self.scsi_id,
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

    pub fn open(&mut self, scsi_id: usize, initial_path: &Path) {
        let filename = format!("hdd{}.img", scsi_id);
        let full_path = initial_path.join(&filename);
        self.scsi_id = scsi_id;
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
