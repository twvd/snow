use std::fs;
use std::path::PathBuf;

use crate::emulator::EmulatorInitArgs;
use anyhow::{anyhow, bail, Result};
use eframe::egui;
use egui_file_dialog::FileDialog;
use sha2::{Digest, Sha256};
use snow_core::emulator::MouseMode;
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{MacModel, MacMonitor};
use strum::IntoEnumIterator;

/// Dialog for selecting Macintosh model and associated ROMs
pub struct ModelSelectionDialog {
    open: bool,
    last_roms: Vec<(MacModel, PathBuf)>,
    last_display_roms: Vec<(MacModel, PathBuf)>,
    selected_model: MacModel,
    init_args: EmulatorInitArgs,
    selected_monitor: MacMonitor,
    early_800k: bool,

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

fn format_ram(sz: usize) -> String {
    if sz < 1024 * 1024 {
        format!("{} KB", sz / 1024)
    } else {
        format!("{} MB", sz / 1024 / 1024)
    }
}

pub struct ModelSelectionResult {
    pub model: MacModel,
    pub main_rom_path: PathBuf,
    pub display_rom_path: Option<PathBuf>,
    pub pram_path: Option<PathBuf>,
    pub extension_rom_path: Option<PathBuf>,
    pub init_args: EmulatorInitArgs,
}

impl Default for ModelSelectionDialog {
    fn default() -> Self {
        Self {
            open: false,
            last_roms: vec![],
            last_display_roms: vec![],
            selected_model: MacModel::Plus,
            init_args: Default::default(),
            selected_monitor: MacMonitor::default(),
            early_800k: false,

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
                .show_pinned_folders(false)
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
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            display_rom_required: false,

            pram_enabled: false,
            pram_dialog: FileDialog::new()
                .add_save_extension("PRAM files", "pram")
                .default_save_extension("PRAM files")
                .show_pinned_folders(false)
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
    pub fn open(
        &mut self,
        last_roms: Vec<(MacModel, PathBuf)>,
        last_display_roms: Vec<(MacModel, PathBuf)>,
    ) {
        self.open = true;
        self.last_roms = last_roms;
        self.last_display_roms = last_display_roms;
        self.error_message.clear();
        self.result = None;

        self.do_model_changed();
    }

    fn do_model_changed(&mut self) {
        // Reset ROM validation when dialog opens or model changes
        self.main_rom_valid = false;
        self.display_rom_valid = false;
        self.update_display_rom_requirement();

        self.init_args.ram_size = None;

        // Load from last used ROMs if possible
        if let Some((_, path)) = self
            .last_roms
            .iter()
            .find(|(m, _)| *m == self.selected_model)
        {
            self.main_rom_path = path.to_string_lossy().to_string();
        }
        if self.display_rom_required {
            if let Some((_, path)) = self
                .last_display_roms
                .iter()
                .find(|(m, _)| *m == self.selected_model)
            {
                self.display_rom_path = path.to_string_lossy().to_string();
            }
        }

        self.do_validate_roms();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_result(&mut self) -> Option<ModelSelectionResult> {
        self.result.take()
    }

    fn update_display_rom_requirement(&mut self) {
        self.display_rom_required =
            matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD);
        if !self.display_rom_required {
            self.display_rom_path.clear();
            self.display_rom_valid = false;
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
        if self.selected_model.is_valid_rom(&rom_data) {
            self.main_rom_valid = true;
            Ok(())
        } else if let Some(model) = MacModel::detect_from_rom(&rom_data) {
            bail!(
                "ROM is for '{}' but '{}' was selected",
                model,
                self.selected_model
            )
        } else {
            bail!("Unknown or unsupported ROM file")
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
            });
            egui::Grid::new("model_grid_mem").show(ui, |ui| {
                ui.label("RAM size:");
                egui::ComboBox::new(egui::Id::new("ram size"), "")
                    .selected_text(format_ram(
                        self.init_args
                            .ram_size
                            .unwrap_or_else(|| self.selected_model.ram_size_default()),
                    ))
                    .show_ui(ui, |ui| {
                        for &sz in self.selected_model.ram_size_options() {
                            ui.selectable_value(
                                &mut self.init_args.ram_size,
                                Some(sz),
                                format_ram(sz),
                            );
                        }
                    });
                ui.end_row();
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
                        ui.horizontal(|ui| {
                            ui.label("Mouse emulation:");
                            egui::ComboBox::new(egui::Id::new("mouse_mode"), "")
                                .selected_text(format!("{}", self.init_args.mouse_mode))
                                .show_ui(ui, |ui| {
                                    for v in MouseMode::iter() {
                                        ui.selectable_value(
                                            &mut self.init_args.mouse_mode,
                                            v,
                                            v.to_string(),
                                        );
                                    }
                                });
                        });
                        if matches!(
                            self.selected_model,
                            MacModel::Early128K | MacModel::Early512K
                        ) {
                            ui.checkbox(
                                &mut self.early_800k,
                                "Use 800K floppy drive on Macintosh 128K/512K",
                            );
                        }
                        if matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD) {
                            ui.checkbox(
                                &mut self.init_args.pmmu_enabled,
                                "Enable PMMU (experimental!)",
                            );
                        }
                        ui.checkbox(&mut self.init_args.audio_disabled, "Disable audio");
                        ui.checkbox(
                            &mut self.disable_rom_validation,
                            "Disable ROM validation (allow loading any ROM)",
                        );
                        ui.checkbox(
                            &mut self.init_args.start_fastforward,
                            "Start in fast-forward mode",
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
                        if !matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD) {
                            self.init_args.pmmu_enabled = false;
                        }

                        self.result = Some(ModelSelectionResult {
                            model: self.selected_model,
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
                                // Deprecated
                                mouse_disabled: None,
                                override_fdd_type: if matches!(
                                    self.selected_model,
                                    MacModel::Early128K | MacModel::Early512K
                                ) && self.early_800k
                                {
                                    Some(DriveType::GCR800KPWM)
                                } else {
                                    None
                                },
                                ..self.init_args
                            },
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
            self.do_model_changed();
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
