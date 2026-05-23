use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::dialogs::filedialog::SnowFileDialog;
use crate::emulator::EmulatorInitArgs;
use crate::settings::AppSettings;
use anyhow::{Result, anyhow, bail};
use eframe::egui;
use sha2::{Digest, Sha256};
use snow_core::emulator::MouseMode;
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{MacModel, MacMonitor, NubusDeviceKind};
use strum::IntoEnumIterator;

/// Video cards selectable for NuBus-equipped models.
const SELECTABLE_VIDEO_CARDS: &[NubusDeviceKind] = &[NubusDeviceKind::Mdc12, NubusDeviceKind::Toby];

/// Video cards that support a user-selectable monitor.
const VIDEO_CARDS_WITH_MONITOR: &[NubusDeviceKind] = &[NubusDeviceKind::Mdc12];

/// Dialog for selecting Macintosh model and associated ROMs
pub struct ModelSelectionDialog {
    open: bool,
    last_roms: Vec<(MacModel, PathBuf)>,
    roms_by_card: Vec<(NubusDeviceKind, PathBuf)>,
    selected_model: MacModel,
    init_args: EmulatorInitArgs,
    selected_monitor: MacMonitor,
    early_800k: bool,

    // Main ROM selection
    main_rom_path: String,
    main_rom_valid: bool,
    main_rom_dialog: SnowFileDialog,

    // Display Card ROM (for applicable models)
    display_rom_path: String,
    display_rom_valid: bool,
    display_rom_dialog: SnowFileDialog,
    display_rom_required: bool,

    // Selected NuBus video card and per-card remembered ROM paths
    selected_video_card: NubusDeviceKind,
    card_rom_paths: HashMap<NubusDeviceKind, String>,

    // PRAM path
    pram_enabled: bool,
    pram_path: String,
    pram_dialog: SnowFileDialog,

    // Extension ROM path
    extension_rom_path: String,
    extension_rom_dialog: SnowFileDialog,

    // Overclock (only takes effect if overclock is checked and value is valid)
    overclock_text: String,

    // Result
    result: Option<ModelSelectionResult>,

    // ROM validation bypass
    disable_rom_validation: bool,

    // Error state
    main_rom_error: String,
    display_rom_error: String,
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
            roms_by_card: vec![],
            selected_model: MacModel::Plus,
            init_args: Default::default(),
            selected_monitor: MacMonitor::default(),
            early_800k: false,

            main_rom_path: String::new(),
            main_rom_valid: false,
            main_rom_dialog: SnowFileDialog::new()
                .add_filter("ROM files", &["rom", "bin"])
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),

            display_rom_path: String::new(),
            display_rom_valid: false,
            display_rom_dialog: SnowFileDialog::new()
                .add_filter("ROM files", &["rom", "bin", "uk6"])
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            display_rom_required: false,
            selected_video_card: NubusDeviceKind::default(),
            card_rom_paths: HashMap::new(),

            pram_enabled: false,
            pram_dialog: SnowFileDialog::new()
                .add_save_extension("PRAM files", "pram")
                .default_save_extension("PRAM files")
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            pram_path: String::new(),

            extension_rom_path: String::new(),
            extension_rom_dialog: SnowFileDialog::new()
                .add_filter("ROM files", &["rom", "bin"])
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),

            overclock_text: "40".into(),

            result: None,
            disable_rom_validation: false,
            main_rom_error: String::new(),
            display_rom_error: String::new(),
        }
    }
}

impl ModelSelectionDialog {
    pub fn open(
        &mut self,
        last_roms: Vec<(MacModel, PathBuf)>,
        roms_by_card: Vec<(NubusDeviceKind, PathBuf)>,
    ) {
        self.open = true;
        self.last_roms = last_roms;
        self.roms_by_card = roms_by_card;
        self.main_rom_error.clear();
        self.display_rom_error.clear();
        self.result = None;

        self.do_model_changed();
    }

    /// Overrides the dialog's current values before opening
    #[allow(clippy::too_many_arguments)]
    pub fn prefill(
        &mut self,
        model: Option<MacModel>,
        main_rom_path: Option<&Path>,
        display_rom_path: Option<&Path>,
        pram_path: Option<&Path>,
        extension_rom_path: Option<&Path>,
        init_args: &EmulatorInitArgs,
    ) {
        if let Some(model) = model {
            self.selected_model = model;
        }
        self.selected_video_card = init_args.video_card;
        if let Some(monitor) = init_args.monitor {
            self.selected_monitor = monitor;
        }
        self.early_800k = init_args.override_fdd_type == Some(DriveType::GCR800KPWM);
        self.init_args = init_args.clone();

        // Recompute model-dependent state (e.g. whether a display ROM is required).
        self.update_display_rom_requirement();

        if let Some(p) = main_rom_path {
            self.main_rom_path = p.to_string_lossy().to_string();
        }
        if let Some(p) = display_rom_path {
            self.display_rom_path = p.to_string_lossy().to_string();
            self.card_rom_paths
                .insert(self.selected_video_card, self.display_rom_path.clone());
        }
        self.pram_enabled = pram_path.is_some();
        self.pram_path = pram_path
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        self.extension_rom_path = extension_rom_path
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        self.do_validate_roms();
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
        self.card_rom_paths.clear();
        for (card, path) in &self.roms_by_card {
            self.card_rom_paths
                .insert(*card, path.to_string_lossy().to_string());
        }
        self.display_rom_path = self
            .card_rom_paths
            .get(&self.selected_video_card)
            .cloned()
            .unwrap_or_default();

        self.do_validate_roms();
    }

    /// Called when the selected video card changes
    fn do_card_changed(&mut self, previous_card: NubusDeviceKind) {
        self.card_rom_paths
            .insert(previous_card, self.display_rom_path.clone());
        self.display_rom_path = self
            .card_rom_paths
            .get(&self.selected_video_card)
            .cloned()
            .unwrap_or_default();
        self.do_validate_display_rom();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_result(&mut self) -> Option<ModelSelectionResult> {
        self.result.take()
    }

    fn update_display_rom_requirement(&mut self) {
        self.display_rom_required = matches!(
            self.selected_model,
            MacModel::MacII
                | MacModel::MacIIFDHD
                | MacModel::MacIIx
                | MacModel::MacIIcx
                | MacModel::SE30
        );
        if !self.display_rom_required {
            self.display_rom_path.clear();
            self.display_rom_valid = false;
        }
    }

    fn video_rom_description(&self) -> &str {
        if self.selected_model == MacModel::SE30 {
            "SE/30 video ROM"
        } else {
            match self.selected_video_card {
                NubusDeviceKind::Toby => "Toby video card ROM (342-0008)",
                NubusDeviceKind::Mdc12 => "Display Card 8-24 ROM (341-0868)",
                _ => todo!(),
            }
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
                .map_err(|e| anyhow!("Cannot read {}: {}", self.video_rom_description(), e))?;
            self.display_rom_valid = true;
            return Ok(());
        }

        // Validate checksum
        let mut hash = Sha256::new();
        hash.update(
            std::fs::read(&self.display_rom_path)
                .map_err(|e| anyhow!("Invalid {}: {}", self.video_rom_description(), e))?,
        );
        let digest = hash.finalize();

        // Expected hash for the selected video card.
        let expected: [u8; 32] = if self.selected_model == MacModel::SE30 {
            hex_literal::hex!("8af892fd7fff89c2151bb3027f3dc61e531f24e4adb0e3face95c90daece4409")
        } else {
            match self.selected_video_card {
                NubusDeviceKind::Toby => hex_literal::hex!(
                    "02261d5b8739352ead945de0fdccd3a364890fb67d75eb5449e4bd19e18c06fc"
                ),
                NubusDeviceKind::Mdc12 => hex_literal::hex!(
                    "e2e763a6b432c9196f619a9f90107726ab1a84a1d54242fe5f5182bf3c97b238"
                ),
                _ => todo!(),
            }
        };

        if digest[..] == expected {
            self.display_rom_valid = true;
            Ok(())
        } else {
            self.display_rom_valid = false;
            bail!("Invalid {}.", self.video_rom_description())
        }
    }

    pub fn update(&mut self, ctx: &egui::Context, frame: &eframe::Frame, settings: &AppSettings) {
        if !self.open {
            return;
        }

        // Update file dialogs
        self.main_rom_dialog.update(ctx, frame);
        self.display_rom_dialog.update(ctx, frame);
        self.pram_dialog.update(ctx, frame);
        self.extension_rom_dialog.update(ctx, frame);

        if *self.main_rom_dialog.state() == egui_file_dialog::DialogState::Open
            || *self.display_rom_dialog.state() == egui_file_dialog::DialogState::Open
            || *self.pram_dialog.state() == egui_file_dialog::DialogState::Open
            || *self.extension_rom_dialog.state() == egui_file_dialog::DialogState::Open
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
                        self.main_rom_dialog.pick_file(settings.native_file_dialogs);
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

                // Display Card ROM selection
                if self.display_rom_required {
                    // Video card selection
                    if self.selected_model != MacModel::SE30 {
                        ui.label("Video card");
                        let prev_card = self.selected_video_card;
                        egui::ComboBox::new(egui::Id::new("Select video card"), "")
                            .selected_text(format!("{}", self.selected_video_card))
                            .show_ui(ui, |ui| {
                                for &card in SELECTABLE_VIDEO_CARDS {
                                    ui.selectable_value(
                                        &mut self.selected_video_card,
                                        card,
                                        card.to_string(),
                                    );
                                }
                            });
                        ui.label("");
                        ui.end_row();
                        if prev_card != self.selected_video_card {
                            self.do_card_changed(prev_card);
                        }
                    }

                    ui.label(self.video_rom_description());
                    ui.horizontal(|ui| {
                        if ui
                            .text_edit_singleline(&mut self.display_rom_path)
                            .lost_focus()
                        {
                            self.do_validate_display_rom();
                        }
                        if ui.button("Browse...").clicked() {
                            self.display_rom_dialog
                                .pick_file(settings.native_file_dialogs);
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

                    // Monitors dropdown
                    if self.selected_model != MacModel::SE30
                        && VIDEO_CARDS_WITH_MONITOR.contains(&self.selected_video_card)
                    {
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
                }
                if matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD) {
                    ui.checkbox(&mut self.init_args.pmmu_enabled, "Enable 68851 PMMU");
                }
            });

            ui.separator();
            egui::Grid::new("model_grid_mouse").show(ui, |ui| {
                ui.label("Mouse emulation:");
                egui::ComboBox::new(egui::Id::new("mouse_mode"), "")
                    .selected_text(format!("{}", self.init_args.mouse_mode))
                    .show_ui(ui, |ui| {
                        for v in MouseMode::iter() {
                            ui.selectable_value(&mut self.init_args.mouse_mode, v, v.to_string());
                        }
                    });
                ui.end_row();
            });
            if self.init_args.mouse_mode == MouseMode::Absolute {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(25, 38, 55))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(70, 100, 140),
                    ))
                    .inner_margin(egui::Margin::same(8))
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} Absolute mode relies on memory patching and only works with \
                                 standard Macintosh System software. It will not work with A/UX \
                                 or other non-standard operating systems; use relative mode for \
                                 those.",
                                egui_material_icons::icons::ICON_INFO
                            ))
                            .color(egui::Color32::from_rgb(150, 190, 230)),
                        );
                    });
            }

            ui.collapsing("Advanced", |ui| {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.pram_enabled, "Persist PRAM");
                        if self.pram_enabled {
                            ui.horizontal(|ui| {
                                ui.text_edit_singleline(&mut self.pram_path);
                                if ui.button("Browse...").clicked() {
                                    self.pram_dialog.save_file(settings.native_file_dialogs);
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
                            self.extension_rom_dialog
                                .pick_file(settings.native_file_dialogs);
                        }
                    });
                });
                ui.group(|ui| {
                    ui.vertical(|ui| {
                        if matches!(
                            self.selected_model,
                            MacModel::Early128K | MacModel::Early512K
                        ) {
                            ui.checkbox(
                                &mut self.early_800k,
                                "Use 800K floppy drive on Macintosh 128K/512K",
                            );
                        }
                        ui.checkbox(
                            &mut self.init_args.audio_disabled,
                            "Disable audio (sync to video)",
                        );
                        ui.checkbox(
                            &mut self.disable_rom_validation,
                            "Disable ROM validation (allow loading any ROM)",
                        );
                        ui.checkbox(
                            &mut self.init_args.start_fastforward,
                            "Start in fast-forward mode",
                        );
                        let mut overclock_enable = self.init_args.overclock.is_some();
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut overclock_enable, "Overclock:");
                            ui.text_edit_singleline(&mut self.overclock_text);
                            ui.label("MHz");
                        });
                        if overclock_enable
                            && let Ok(overclock_value) = self.overclock_text.parse::<f64>()
                            && overclock_value > 0.0
                        {
                            self.init_args.overclock = Some((overclock_value * 1_000_000.0) as u64);
                        } else {
                            self.init_args.overclock = None;
                        }
                        if self.selected_model == MacModel::SE30 {
                            let mut x = false;
                            ui.checkbox(&mut x, "Emulate exploded PRAM battery");
                        }
                    });
                });
            });

            // Error messages
            if !self.main_rom_error.is_empty() || !self.display_rom_error.is_empty() {
                ui.separator();
                ui.add_space(10.0);
                if !self.main_rom_error.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "    {} {}",
                            egui_material_icons::icons::ICON_ERROR,
                            &self.main_rom_error
                        ))
                        .color(egui::Color32::RED),
                    );
                }
                if !self.display_rom_error.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "    {} {}",
                            egui_material_icons::icons::ICON_ERROR,
                            &self.display_rom_error
                        ))
                        .color(egui::Color32::RED),
                    );
                }
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
                                video_card: self.selected_video_card,
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
            self.main_rom_error = e.to_string();
        } else {
            self.main_rom_error.clear();
        }
    }

    fn do_validate_display_rom(&mut self) {
        if let Err(e) = self.validate_display_rom() {
            self.display_rom_error = e.to_string();
        } else {
            self.display_rom_error.clear();
        }
    }
}
