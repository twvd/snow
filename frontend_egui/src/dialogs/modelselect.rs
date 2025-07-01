use std::fs;
use std::path::PathBuf;

use crate::emulator::EmulatorInitArgs;
use anyhow::{anyhow, bail, Result};
use eframe::egui;
use egui_file_dialog::FileDialog;
use sha2::{Digest, Sha256};
use snow_core::mac::{MacModel, MacMonitor};
use strum::IntoEnumIterator;

/// Dialog for selecting Macintosh model and associated ROMs
pub struct ModelSelectionDialog {
    open: bool,
    selected_model: MacModel,
    memory_size: usize,
    init_args: EmulatorInitArgs,
    selected_monitor: MacMonitor,

    // Main ROM selection
    main_rom_path: String,
    main_rom_valid: bool,
    main_rom_dialog: FileDialog,

    // Display Card ROM (for Mac II only)
    display_rom_path: String,
    display_rom_valid: bool,
    display_rom_dialog: FileDialog,
    display_rom_required: bool,

    // PRAM path
    pram_enabled: bool,
    pram_path: String,
    pram_dialog: FileDialog,

    // Extension ROM path
    extension_rom_path: String,
    extension_rom_dialog: FileDialog,

    // Result
    result: Option<ModelSelectionResult>,

    // ROM validation bypass
    disable_rom_validation: bool,

    // Error state
    error_message: String,
}

#[allow(dead_code)]
pub struct ModelSelectionResult {
    pub model: MacModel,
    pub memory_size: usize,
    pub main_rom_path: PathBuf,
    pub display_rom_path: Option<PathBuf>,
    pub pram_path: Option<PathBuf>,
    pub extension_rom_path: Option<PathBuf>,
    pub init_args: EmulatorInitArgs,
    pub disable_rom_validation: bool,
}

impl Default for ModelSelectionDialog {
    fn default() -> Self {
        Self {
            open: false,
            selected_model: MacModel::Plus,
            memory_size: 4 * 1024 * 1024, // 4MB default
            init_args: Default::default(),
            selected_monitor: MacMonitor::default(),

            main_rom_path: String::new(),
            main_rom_valid: false,
            main_rom_dialog: FileDialog::new()
                .add_file_filter(
                    "ROM files (*.rom, *.bin)",
                    std::sync::Arc::new(|p| {
                        if let Some(ext) = p.extension() {
                            let ext_str = ext.to_string_lossy().to_lowercase();
                            ext_str == "rom" || ext_str == "bin"
                        } else {
                            false
                        }
                    }),
                )
                .default_file_filter("ROM files (*.rom, *.bin)")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),

            display_rom_path: String::new(),
            display_rom_valid: false,
            display_rom_dialog: FileDialog::new()
                .add_file_filter(
                    "ROM files (*.rom, *.bin)",
                    std::sync::Arc::new(|p| {
                        if let Some(ext) = p.extension() {
                            let ext_str = ext.to_string_lossy().to_lowercase();
                            ext_str == "rom" || ext_str == "bin"
                        } else {
                            false
                        }
                    }),
                )
                .default_file_filter("ROM files (*.rom, *.bin)")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            display_rom_required: false,

            pram_enabled: false,
            pram_dialog: FileDialog::new()
                .add_save_extension("PRAM files", "pram")
                .default_save_extension("PRAM files")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            pram_path: String::new(),

            extension_rom_path: String::new(),
            extension_rom_dialog: FileDialog::new()
                .add_file_filter(
                    "ROM files (*.rom, *.bin)",
                    std::sync::Arc::new(|p| {
                        if let Some(ext) = p.extension() {
                            let ext_str = ext.to_string_lossy().to_lowercase();
                            ext_str == "rom" || ext_str == "bin"
                        } else {
                            false
                        }
                    }),
                )
                .default_file_filter("ROM files (*.rom, *.bin)")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),

            result: None,
            disable_rom_validation: false,
            error_message: String::new(),
        }
    }
}

impl ModelSelectionDialog {
    pub fn open(&mut self) {
        self.open = true;
        self.error_message.clear();
        self.result = None;

        // Reset ROM validation when dialog opens
        self.main_rom_valid = false;
        self.display_rom_valid = false;
        self.update_memory_options();
        self.update_display_rom_requirement();
        self.do_validate_roms();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_result(&mut self) -> Option<ModelSelectionResult> {
        self.result.take()
    }

    fn update_memory_options(&mut self) {
        // Set default memory size based on model
        self.memory_size = match self.selected_model {
            MacModel::Early128K => 128 * 1024,
            MacModel::Early512K => 512 * 1024,
            MacModel::Plus | MacModel::SE | MacModel::SeFdhd | MacModel::Classic => 4 * 1024 * 1024,
            MacModel::MacII | MacModel::MacIIFDHD => 8 * 1024 * 1024,
        };
    }

    fn update_display_rom_requirement(&mut self) {
        self.display_rom_required =
            matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD);
        if !self.display_rom_required {
            self.display_rom_path.clear();
            self.display_rom_valid = false;
        }
    }

    #[allow(dead_code, clippy::identity_op)]
    fn get_memory_options(model: MacModel) -> Vec<(String, usize)> {
        // TODO
        match model {
            MacModel::Early128K => vec![("128KB".to_string(), 128 * 1024)],
            MacModel::Early512K => vec![("512KB".to_string(), 512 * 1024)],
            MacModel::Plus => vec![
                ("1MB".to_string(), 1 * 1024 * 1024),
                ("2MB".to_string(), 2 * 1024 * 1024),
                ("4MB".to_string(), 4 * 1024 * 1024),
            ],
            MacModel::SE | MacModel::SeFdhd => vec![
                ("1MB".to_string(), 1 * 1024 * 1024),
                ("2MB".to_string(), 2 * 1024 * 1024),
                ("4MB".to_string(), 4 * 1024 * 1024),
            ],
            MacModel::Classic => vec![
                ("2MB".to_string(), 2 * 1024 * 1024),
                ("4MB".to_string(), 4 * 1024 * 1024),
            ],
            MacModel::MacII | MacModel::MacIIFDHD => vec![
                ("1MB".to_string(), 1 * 1024 * 1024),
                ("2MB".to_string(), 2 * 1024 * 1024),
                ("4MB".to_string(), 4 * 1024 * 1024),
                ("8MB".to_string(), 8 * 1024 * 1024),
                ("16MB".to_string(), 16 * 1024 * 1024),
                ("32MB".to_string(), 32 * 1024 * 1024),
            ],
        }
    }

    fn validate_main_rom(&mut self) -> Result<()> {
        self.main_rom_valid = false;
        if self.main_rom_path.is_empty() {
            return Ok(());
        }

        // Skip validation if disabled
        if self.disable_rom_validation {
            // Just check if the file exists and is readable
            let _rom_data = fs::read(&self.main_rom_path)?;
            self.main_rom_valid = true;
            return Ok(());
        }

        let rom_data = fs::read(&self.main_rom_path)?;

        // Check ROM checksum
        let detected_model = MacModel::detect_from_rom(&rom_data);

        match detected_model {
            Some(model) if model == self.selected_model => {
                self.main_rom_valid = true;
                Ok(())
            }
            Some(model) => {
                bail!(
                    "ROM is for '{}' but '{}' was selected",
                    model,
                    self.selected_model
                )
            }
            None => {
                bail!("Unknown or unsupported ROM file")
            }
        }
    }

    fn validate_display_rom(&mut self) -> Result<()> {
        if !self.display_rom_required {
            self.display_rom_valid = true;
            return Ok(());
        }

        self.display_rom_valid = false;
        if self.display_rom_path.is_empty() {
            return Ok(());
        }

        // Skip validation if disabled
        if self.disable_rom_validation {
            // Just check if the file exists and is readable
            let _rom_data = std::fs::read(&self.display_rom_path)
                .map_err(|e| anyhow!("Cannot read Display Card ROM: {}", e))?;
            self.display_rom_valid = true;
            return Ok(());
        }

        // Validate checksum
        let mut hash = Sha256::new();
        hash.update(
            std::fs::read(&self.display_rom_path)
                .map_err(|e| anyhow!("Invalid Display Card ROM: {}", e))?,
        );
        let digest = hash.finalize();

        if digest[..]
            == hex_literal::hex!("e2e763a6b432c9196f619a9f90107726ab1a84a1d54242fe5f5182bf3c97b238")
        {
            self.display_rom_valid = true;
            Ok(())
        } else {
            self.display_rom_valid = false;
            bail!("Invalid Display Card ROM. Expected Macintosh Display Card 8-24 (341-0868) ROM.")
        }
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        // Update file dialogs
        self.main_rom_dialog.update(ctx);
        self.display_rom_dialog.update(ctx);
        self.pram_dialog.update(ctx);
        self.extension_rom_dialog.update(ctx);

        if self.main_rom_dialog.state() == egui_file_dialog::DialogState::Open
            || self.display_rom_dialog.state() == egui_file_dialog::DialogState::Open
            || self.pram_dialog.state() == egui_file_dialog::DialogState::Open
            || self.extension_rom_dialog.state() == egui_file_dialog::DialogState::Open
        {
            return;
        }

        // Handle file dialog results
        if let Some(path) = self.main_rom_dialog.take_picked() {
            self.main_rom_path = path.to_string_lossy().to_string();
            self.do_validate_main_rom();
        }

        if let Some(path) = self.display_rom_dialog.take_picked() {
            self.display_rom_path = path.to_string_lossy().to_string();
            self.do_validate_display_rom();
        }

        if let Some(path) = self.pram_dialog.take_picked() {
            self.pram_path = path.to_string_lossy().to_string();
        }

        if let Some(path) = self.extension_rom_dialog.take_picked() {
            self.extension_rom_path = path.to_string_lossy().to_string();
        }

        // Main dialog window
        let last_model = self.selected_model;
        let last_validation_disabled = self.disable_rom_validation;
        egui::Modal::new(egui::Id::new("Load ROM")).show(ctx, |ui| {
            ui.style_mut().spacing.item_spacing = egui::Vec2::splat(4.0);
            ui.set_width(700.0);

            ui.heading("Set up emulated system");
            ui.separator();

            // Model selection
            egui::Grid::new("model_grid_1").show(ui, |ui| {
                ui.label("Macintosh model:");
                egui::ComboBox::new(egui::Id::new("Select Macintosh model"), "")
                    .selected_text(format!("{}", self.selected_model))
                    .show_ui(ui, |ui| {
                        for model in MacModel::iter() {
                            ui.selectable_value(&mut self.selected_model, model, model.to_string());
                        }
                    });
                ui.end_row();

                // Memory selection
                //ui.label("Memory size:");
                //let memory_options = Self::get_memory_options(self.selected_model);
                //let current_memory_str = memory_options
                //    .iter()
                //    .find(|(_, size)| *size == self.memory_size)
                //    .map(|(name, _)| name.as_str())
                //    .unwrap_or("Custom");

                //egui::ComboBox::new(egui::Id::new("Select memory size"), "")
                //    .selected_text(current_memory_str)
                //    .show_ui(ui, |ui| {
                //        for (name, size) in memory_options {
                //            ui.selectable_value(&mut self.memory_size, size, name);
                //        }
                //    });
                //ui.end_row();
            });

            ui.separator();
            ui.label(egui::RichText::from("Select ROM files").strong());
            egui::Grid::new("model_grid_2").show(ui, |ui| {
                // Main ROM selection
                ui.label("System ROM");
                ui.horizontal(|ui| {
                    if ui
                        .text_edit_singleline(&mut self.main_rom_path)
                        .lost_focus()
                    {
                        self.do_validate_main_rom();
                    }
                    if ui.button("Browse...").clicked() {
                        self.main_rom_dialog.pick_file();
                    }
                });

                // Validation indicator
                if !self.main_rom_path.is_empty() {
                    let (icon, color) = if self.main_rom_valid {
                        (
                            egui_material_icons::icons::ICON_CHECK_CIRCLE,
                            egui::Color32::DARK_GREEN,
                        )
                    } else {
                        (
                            egui_material_icons::icons::ICON_ERROR,
                            egui::Color32::DARK_RED,
                        )
                    };
                    ui.label(egui::RichText::new(icon).color(color));
                } else {
                    ui.label("");
                }
                ui.end_row();

                // Display Card ROM selection (Mac II only)
                if self.display_rom_required {
                    ui.label("Macintosh Display Card 8-24 ROM (341-0868)");
                    ui.horizontal(|ui| {
                        if ui
                            .text_edit_singleline(&mut self.display_rom_path)
                            .lost_focus()
                        {
                            self.do_validate_display_rom();
                        }
                        if ui.button("Browse...").clicked() {
                            self.display_rom_dialog.pick_file();
                        }
                    });

                    // Validation indicator
                    if !self.display_rom_path.is_empty() {
                        let (icon, color) = if self.display_rom_valid {
                            (
                                egui_material_icons::icons::ICON_CHECK_CIRCLE,
                                egui::Color32::DARK_GREEN,
                            )
                        } else {
                            (
                                egui_material_icons::icons::ICON_ERROR,
                                egui::Color32::DARK_RED,
                            )
                        };
                        ui.label(egui::RichText::new(icon).color(color));
                    } else {
                        ui.label("");
                    }
                    ui.end_row();

                    ui.label(egui::RichText::from("Select peripherals").strong());
                    ui.end_row();

                    ui.label("Monitor");
                    egui::ComboBox::new(egui::Id::new("Select monitor"), "")
                        .selected_text(format!("{}", self.selected_monitor))
                        .show_ui(ui, |ui| {
                            for monitor in MacMonitor::iter() {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    monitor,
                                    monitor.to_string(),
                                );
                            }
                        });
                    ui.end_row();
                }
            });

            ui.collapsing("Advanced", |ui| {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.pram_enabled, "Persist PRAM");
                        if self.pram_enabled {
                            ui.horizontal(|ui| {
                                ui.text_edit_singleline(&mut self.pram_path);
                                if ui.button("Browse...").clicked() {
                                    self.pram_dialog.save_file();
                                }
                            });
                        }
                    });
                });
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Extension ROM:");
                        ui.text_edit_singleline(&mut self.extension_rom_path);
                        if ui.button("Browse...").clicked() {
                            self.extension_rom_dialog.pick_file();
                        }
                    });
                });
                ui.group(|ui| {
                    ui.vertical(|ui| {
                        ui.checkbox(&mut self.init_args.audio_disabled, "Disable audio");
                        ui.checkbox(&mut self.init_args.mouse_disabled, "Disable mouse");
                        ui.checkbox(
                            &mut self.disable_rom_validation,
                            "Disable ROM validation (allow loading any ROM)",
                        );
                    });
                });
            });

            // Error message
            if !self.error_message.is_empty() {
                ui.separator();
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!(
                        "    {} {}",
                        egui_material_icons::icons::ICON_ERROR,
                        &self.error_message
                    ))
                    .color(egui::Color32::RED),
                );
                ui.add_space(10.0);
            }

            ui.separator();

            // Buttons
            egui::Sides::new().show(
                ui,
                |_ui| {},
                |ui| {
                    let can_proceed = self.main_rom_valid
                        && (!self.display_rom_required || self.display_rom_valid);

                    if ui
                        .add_enabled(can_proceed, egui::Button::new("Load and run"))
                        .clicked()
                    {
                        self.result = Some(ModelSelectionResult {
                            model: self.selected_model,
                            memory_size: self.memory_size,
                            main_rom_path: PathBuf::from(&self.main_rom_path),
                            display_rom_path: if self.display_rom_required
                                && !self.display_rom_path.is_empty()
                            {
                                Some(PathBuf::from(&self.display_rom_path))
                            } else {
                                None
                            },
                            pram_path: if self.pram_path.is_empty() {
                                None
                            } else {
                                Some(PathBuf::from(&self.pram_path))
                            },
                            extension_rom_path: if self.extension_rom_path.is_empty() {
                                None
                            } else {
                                Some(PathBuf::from(&self.extension_rom_path))
                            },
                            init_args: EmulatorInitArgs {
                                monitor: if self.display_rom_required {
                                    Some(self.selected_monitor)
                                } else {
                                    None
                                },
                                ..self.init_args
                            },
                            disable_rom_validation: self.disable_rom_validation,
                        });
                        self.open = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.open = false;
                    }
                },
            );
        });

        if last_model != self.selected_model {
            self.update_memory_options();
            self.update_display_rom_requirement();
            self.do_validate_roms();
        }

        if last_validation_disabled != self.disable_rom_validation {
            self.do_validate_roms();
        }
    }

    fn do_validate_roms(&mut self) {
        self.do_validate_display_rom();
        self.do_validate_main_rom();
    }

    fn do_validate_main_rom(&mut self) {
        if self.main_rom_path.is_empty() {
            return;
        }

        if let Err(e) = self.validate_main_rom() {
            self.error_message = e.to_string();
        } else {
            self.error_message.clear();
        }
    }

    fn do_validate_display_rom(&mut self) {
        if (self.display_rom_path.is_empty() || !self.display_rom_required)
            && self.error_message.starts_with("Invalid Display Card")
        {
            self.error_message.clear();
        }

        if let Err(e) = self.validate_display_rom() {
            self.error_message = e.to_string();
        } else if !self.main_rom_path.is_empty()
            && self.error_message.starts_with("Invalid Display Card")
        {
            self.error_message.clear();
        }
    }
}
