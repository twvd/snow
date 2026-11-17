use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::dialogs::filedialog::SnowFileDialog;
use crate::emulator::EmulatorInitArgs;
use crate::settings::AppSettings;
use anyhow::{Result, anyhow, bail};
use eframe::egui;
use sha2::{Digest, Sha256};
use snow_core::cpu_m68k::M68040;
use snow_core::devices::{
    Bus, BusKind, Device, DeviceConfig, DeviceModel, DisplayInfo, FunctionConfig, FunctionInfo,
    ModelInfo, RomInfo, bus_id, device_configs, models_for,
};
use snow_core::emulator::MouseMode;
use snow_core::mac::maclc::pds_iie::IieDiskDevice;
use snow_core::mac::{MacModel, MacMonitor};
use strum::IntoEnumIterator;

/// The Config of a new device of a device model, with nothing chosen: whatever its function, a
/// built-in device or one on any other bus that the dialog describes
fn default_config(info: &ModelInfo) -> FunctionConfig {
    info.function.default_config()
}

/// Whether the dialog describes the devices of a bus: those that are part of the machine, and
/// are never added or removed while it runs
fn describes(bus: &Bus, mac: MacModel) -> bool {
    models_for(bus.kind, mac).any(|info| info.fixed)
}

/// The check of the ROM file of a device
struct RomCheck {
    valid: bool,
    /// What is wrong with the file, for the user
    error: String,
}

/// The name of a device on the Apple IIe Card's external disk port
fn iie_disk_device_name(device: IieDiskDevice) -> &'static str {
    match device {
        IieDiskDevice::None => "None",
        IieDiskDevice::Apple525 => "Apple 5.25 Drive",
        IieDiskDevice::UniDisk35 => "UniDisk 3.5",
    }
}

/// Dialog for selecting Macintosh model and associated ROMs
pub struct ModelSelectionDialog {
    open: bool,
    last_roms: Vec<(MacModel, PathBuf)>,
    /// The ROM file last used for each device model that needs one
    device_roms: Vec<(DeviceModel, PathBuf)>,
    selected_model: MacModel,
    init_args: EmulatorInitArgs,

    /// The devices that are part of the machine: one device config for each slot of the
    /// selected model that the dialog describes and that has a device
    devices: Vec<DeviceConfig>,
    /// The monitors chosen, in the order of the displays, kept across a change of Mac model
    monitors: Vec<Option<MacMonitor>>,
    /// The "Use 800K floppy drive" checkbox, kept across a change of Mac model
    drive_800k: bool,

    // Main ROM selection
    main_rom_path: String,
    main_rom_valid: bool,
    main_rom_dialog: SnowFileDialog,

    // ROM of a device, and the bus and slot of the device that the file is being picked for
    device_rom_dialog: SnowFileDialog,
    device_rom_slot: (String, String),
    /// The check of the ROM file of each device that needs one, by the ids of its bus and slot
    rom_checks: HashMap<(String, String), RomCheck>,

    // PRAM path
    pram_enabled: bool,
    pram_path: String,
    pram_dialog: SnowFileDialog,

    // Extension ROM path
    extension_rom_path: String,
    extension_rom_dialog: SnowFileDialog,

    // Overclock (only takes effect if overclock is checked and value is valid)
    overclock_text: String,

    // The Apple IIe Card settings dialog is open
    iie_settings_open: bool,

    // Result
    result: Option<ModelSelectionResult>,

    // ROM validation bypass
    disable_rom_validation: bool,

    // Error state
    main_rom_error: String,
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
    pub pram_path: Option<PathBuf>,
    pub extension_rom_path: Option<PathBuf>,
    pub init_args: EmulatorInitArgs,
    /// The devices that are part of the machine, where they aren't what the Mac model has
    /// without being asked
    pub devices: Vec<DeviceConfig>,
}

impl Default for ModelSelectionDialog {
    fn default() -> Self {
        Self {
            open: false,
            last_roms: vec![],
            device_roms: vec![],
            selected_model: MacModel::Plus,
            init_args: Default::default(),
            devices: vec![],
            monitors: vec![],
            drive_800k: false,

            main_rom_path: String::new(),
            main_rom_valid: false,
            main_rom_dialog: SnowFileDialog::new()
                .add_filter("ROM files", &["rom", "bin"])
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),

            device_rom_dialog: SnowFileDialog::new()
                .add_filter("ROM files", &["rom", "bin", "uk6"])
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            device_rom_slot: Default::default(),
            rom_checks: HashMap::new(),

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
            iie_settings_open: false,

            result: None,
            disable_rom_validation: false,
            main_rom_error: String::new(),
        }
    }
}

impl ModelSelectionDialog {
    pub fn open(
        &mut self,
        last_roms: Vec<(MacModel, PathBuf)>,
        device_roms: Vec<(DeviceModel, PathBuf)>,
    ) {
        self.open = true;
        self.iie_settings_open = false;
        self.last_roms = last_roms;
        self.device_roms = device_roms;
        self.main_rom_error.clear();
        self.result = None;

        self.do_model_changed();
    }

    /// Overrides the dialog's current values before opening. `devices` is the list of device
    /// configs of a machine: the dialog takes those of the devices it describes.
    pub fn prefill(
        &mut self,
        model: Option<MacModel>,
        main_rom_path: Option<&Path>,
        pram_path: Option<&Path>,
        extension_rom_path: Option<&Path>,
        init_args: &EmulatorInitArgs,
        devices: &[DeviceConfig],
    ) {
        if let Some(model) = model {
            self.selected_model = model;
        }
        self.init_args = init_args.clone();
        self.devices = devices.to_vec();
        self.fit_devices();

        if let Some(p) = main_rom_path {
            self.main_rom_path = p.to_string_lossy().to_string();
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

        self.init_args.ram_size = None;

        // Load from last used ROMs if possible
        if let Some((_, path)) = self
            .last_roms
            .iter()
            .find(|(m, _)| *m == self.selected_model)
        {
            self.main_rom_path = path.to_string_lossy().to_string();
        }
        self.remember_choices();
        self.fit_devices();
        self.apply_choices();
        for index in 0..self.devices.len() {
            let Some(info) = self.devices[index].model.map(DeviceModel::info) else {
                continue;
            };
            if info.rom.is_some() {
                self.devices[index].rom = self.remembered_rom(info.model);
            }
            if let FunctionConfig::Display(display) = &mut self.devices[index].function {
                display.vram_size = None;
            }
        }

        self.do_validate_roms();
    }

    /// Notes the monitors and the 800K drive chosen for the Mac model the list of devices is
    /// for, to apply them again to the next one. A Mac model with none of them leaves what was
    /// noted before.
    fn remember_choices(&mut self) {
        let monitors: Vec<_> = self
            .devices
            .iter()
            .filter_map(
                |config| match (config.model?.info().function, &config.function) {
                    (FunctionInfo::Display(info), FunctionConfig::Display(display))
                        if info.monitors.len() > 1 =>
                    {
                        Some(display.monitor)
                    }
                    _ => None,
                },
            )
            .collect();
        if !monitors.is_empty() {
            self.monitors = monitors;
        }
        // The drives are the 800K drive or the 400K drive it replaces only on the Macs that
        // have the checkbox
        let drives: Vec<_> = self
            .devices
            .iter()
            .filter(|config| config.bus == bus_id::SWIM)
            .filter_map(|config| config.model)
            .collect();
        if drives.contains(&DeviceModel::Mac800KPwmDrive) {
            self.drive_800k = true;
        } else if drives.contains(&DeviceModel::Mac400KDrive) {
            self.drive_800k = false;
        }
    }

    /// Gives the displays of the list the monitors noted, in their order, where a video can
    /// have that monitor, and the 800K drive if it was chosen and the Mac model takes it
    fn apply_choices(&mut self) {
        let mut monitors = self.monitors.iter();
        for config in &mut self.devices {
            let Some(FunctionInfo::Display(info)) = config.model.map(|m| m.info().function) else {
                continue;
            };
            let FunctionConfig::Display(display) = &mut config.function else {
                continue;
            };
            if info.monitors.len() <= 1 {
                continue;
            }
            if let Some(&Some(monitor)) = monitors.next()
                && info.monitors.contains(&monitor)
            {
                display.monitor = Some(monitor);
            }
        }
        if self.drive_800k {
            self.set_800k_drives(true);
        }
    }

    /// Makes the list of devices that of the selected model: each slot that the dialog
    /// describes keeps what the list had for it if the model takes it, and gets what the
    /// model has there otherwise. A machine with no video gets a video card in its first
    /// NuBus slot.
    fn fit_devices(&mut self) {
        let mac = self.selected_model;
        let buses = mac.buses();
        let mut fitted = vec![];
        for bus in &buses {
            if !describes(bus, mac) {
                continue;
            }
            for slot in &bus.slots {
                let own = slot.device.as_ref().map(|device| device.model);
                let had = self
                    .devices
                    .iter()
                    .find(|c| c.bus == bus.id && c.slot == slot.id);
                // A device config that names no device model is for the Mac model's own
                let kept = had
                    .and_then(|c| c.model.or(own))
                    .filter(|m| models_for(bus.kind, mac).any(|info| info.model == *m));
                let Some(model) = kept.or(own) else {
                    continue;
                };
                let info = model.info();
                let (rom, function) = match had {
                    Some(c) if kept == Some(model) && info.function.takes(&c.function) => {
                        (c.rom.clone(), c.function.clone())
                    }
                    _ => (None, default_config(info)),
                };
                fitted.push(DeviceConfig {
                    bus: bus.id.clone(),
                    slot: slot.id.clone(),
                    model: Some(model),
                    rom: rom.or_else(|| self.remembered_rom(model)),
                    function,
                });
            }
        }
        // A machine with no video of its own needs a video card
        let video = |c: &DeviceConfig| matches!(c.function, FunctionConfig::Display(_));
        if !fitted.iter().any(video)
            && let Some(bus) = buses.iter().find(|bus| bus.kind == BusKind::Nubus)
            && let Some(info) = models_for(bus.kind, mac)
                .find(|info| matches!(info.function, FunctionInfo::Display(_)))
        {
            fitted.push(DeviceConfig {
                bus: bus.id.clone(),
                slot: bus.slots[0].id.clone(),
                model: Some(info.model),
                rom: self.remembered_rom(info.model),
                function: default_config(info),
            });
        }
        self.devices = fitted;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_result(&mut self) -> Option<ModelSelectionResult> {
        self.result.take()
    }

    /// Where the device config of a slot is in the list, if the slot has a device
    fn config_index(&self, bus: &str, slot: &str) -> Option<usize> {
        self.devices
            .iter()
            .position(|c| c.bus == bus && c.slot == slot)
    }

    /// The ROM file last used for a device model, if it needs one
    fn remembered_rom(&self, model: DeviceModel) -> Option<PathBuf> {
        model.info().rom.as_ref()?;
        let found = self.device_roms.iter().rev().find(|(m, _)| *m == model);
        found.map(|(_, path)| path.clone())
    }

    /// Puts a device of a device model in a slot, or nothing. The ROM file of the device that
    /// was there is remembered for the next device of its device model.
    fn choose(&mut self, bus: &str, slot: &str, model: Option<DeviceModel>) {
        if let Some(index) = self.config_index(bus, slot) {
            let old = self.devices.remove(index);
            if let (Some(model), Some(rom)) = (old.model, old.rom) {
                self.device_roms.push((model, rom));
            }
        }
        if let Some(model) = model {
            let info = model.info();
            self.devices.push(DeviceConfig {
                bus: bus.to_string(),
                slot: slot.to_string(),
                model: Some(model),
                rom: self.remembered_rom(model),
                function: default_config(info),
            });
        }
        self.do_validate_device_roms();
    }

    /// The number of screens of the machine: the videos whose screen is the machine's own,
    /// and those with a monitor on their connector
    fn screens(&self) -> usize {
        let screen = |config: &&DeviceConfig| {
            let info = config.model.map(|model| model.info().function);
            match (info, &config.function) {
                (Some(FunctionInfo::Display(info)), FunctionConfig::Display(display)) => {
                    info.monitors.is_empty() || display.monitor != Some(MacMonitor::NoMonitor)
                }
                _ => false,
            }
        };
        self.devices.iter().filter(screen).count()
    }

    fn has_screen(&self) -> bool {
        self.screens() > 0
    }

    /// The device configs of the machine as it is described: those of the list that aren't
    /// what the Mac model has without being asked
    fn result_devices(&self) -> Vec<DeviceConfig> {
        let mut buses = self.selected_model.buses();
        for bus in &mut buses {
            for slot in &mut bus.slots {
                let config = self
                    .devices
                    .iter()
                    .find(|c| c.bus == bus.id && c.slot == slot.id);
                slot.device = config.and_then(|c| {
                    Some(Device {
                        model: c.model?,
                        rom: c.rom.clone(),
                        function: c.function.clone(),
                        name: None,
                    })
                });
            }
        }
        device_configs(&buses, self.selected_model)
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

    /// Checks the ROM file of a device against what its device model asks for. Returns
    /// whether the device can be made with it.
    fn validate_device_rom(rom: &RomInfo, path: Option<&Path>, any_file: bool) -> Result<bool> {
        let Some(path) = path else {
            return Ok(rom.optional);
        };

        // Skip validation if disabled
        if any_file {
            // Just check if the file exists and is readable
            let _rom_data =
                fs::read(path).map_err(|e| anyhow!("Cannot read {}: {}", rom.label, e))?;
            return Ok(true);
        }

        // Validate checksum
        let mut rom_data = fs::read(path).map_err(|e| anyhow!("Invalid {}: {}", rom.label, e))?;
        // What differs from one unit to the next isn't in the digests
        for byte in &mut rom_data {
            *byte &= !rom.varying_bits;
        }
        let digest = Sha256::digest(&rom_data);
        let digest: String = digest.iter().map(|byte| format!("{:02x}", byte)).collect();
        if rom.digests.contains(&digest.as_str()) {
            Ok(true)
        } else {
            bail!("Invalid {}.", rom.label)
        }
    }

    pub fn update(&mut self, ctx: &egui::Context, frame: &eframe::Frame, settings: &AppSettings) {
        if !self.open {
            return;
        }

        // Update file dialogs
        self.main_rom_dialog.update(ctx, frame);
        self.device_rom_dialog.update(ctx, frame);
        self.pram_dialog.update(ctx, frame);
        self.extension_rom_dialog.update(ctx, frame);

        if *self.main_rom_dialog.state() == egui_file_dialog::DialogState::Open
            || *self.device_rom_dialog.state() == egui_file_dialog::DialogState::Open
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

        if let Some(path) = self.device_rom_dialog.take_picked() {
            let (bus, slot) = &self.device_rom_slot;
            if let Some(index) = self.config_index(bus, slot) {
                self.devices[index].rom = Some(path);
            }
            self.do_validate_device_roms();
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

                // The ROM files of the devices of the logic board, such as the video of an
                // SE/30
                let buses = self.selected_model.buses();
                for bus in buses.iter().filter(|bus| bus.kind == BusKind::BuiltIn) {
                    for slot in &bus.slots {
                        self.rom_row(ui, settings, &bus.id, &slot.id);
                    }
                }
            });

            if self.has_peripherals() {
                ui.separator();
                ui.label(egui::RichText::from("Select peripherals").strong());
                egui::Grid::new("model_grid_3").show(ui, |ui| {
                    self.peripheral_rows(ui, settings);
                });
            }

            ui.separator();
            if self.selected_model.is_lisa() && self.init_args.mouse_mode == MouseMode::Absolute {
                self.init_args.mouse_mode = MouseMode::RelativeHw;
            }
            egui::Grid::new("model_grid_mouse").show(ui, |ui| {
                ui.label("Mouse emulation:");
                egui::ComboBox::new(egui::Id::new("mouse_mode"), "")
                    .selected_text(format!("{}", self.init_args.mouse_mode))
                    .show_ui(ui, |ui| {
                        for v in MouseMode::iter() {
                            // The Lisa's mouse only reports motion
                            let possible =
                                !(self.selected_model.is_lisa() && v == MouseMode::Absolute);
                            ui.add_enabled_ui(possible, |ui| {
                                ui.selectable_value(&mut self.init_args.mouse_mode, v, v.to_string())
                                    .on_disabled_hover_text("The Lisa's mouse only reports motion");
                            });
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
                        self.drive_checkbox(ui);
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
                            && let overclock_mhz = (overclock_value * 1_000_000.0) as u64
                            && overclock_mhz > 0
                        {
                            self.init_args.overclock = Some(overclock_mhz);
                        } else {
                            self.init_args.overclock = None;
                        }
                        if self.selected_model.cpu_type() == M68040 {
                            ui.checkbox(
                                &mut self.init_args.cache_flush_workaround,
                                "Cache flush workaround",
                            );
                            ui.label(
                                egui::RichText::new(
                                    "A data cache flush also clears the instruction cache.\n\
                                     Needed to boot the Mac OS 8.1 CD.",
                                )
                                .small()
                                .weak(),
                            );
                            ui.checkbox(
                                &mut self.init_args.vm_pflush_workaround,
                                "MacOS VM flush workaround",
                            );
                            ui.label(
                                egui::RichText::new(
                                    "A user-mode PFLUSH flushes the ATC instead of trapping.\n\
                                     Works around a Mac OS 8.1 virtual memory bug that loses writes.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                        if self.selected_model == MacModel::SE30 {
                            let mut x = false;
                            ui.checkbox(&mut x, "Emulate exploded PRAM battery");
                        }
                    });
                });
            });

            // Error messages
            let mut errors = vec![];
            if !self.main_rom_error.is_empty() {
                errors.push(self.main_rom_error.clone());
            }
            for config in &self.devices {
                let key = (config.bus.clone(), config.slot.clone());
                if let Some(check) = self.rom_checks.get(&key)
                    && !check.error.is_empty()
                {
                    errors.push(check.error.clone());
                }
            }
            if !self.has_screen() {
                errors.push(
                    "This machine has no screen: none of its videos has a monitor".to_string(),
                );
            }
            if !errors.is_empty() {
                ui.separator();
                ui.add_space(10.0);
                for error in &errors {
                    ui.label(
                        egui::RichText::new(format!(
                            "    {} {}",
                            egui_material_icons::icons::ICON_ERROR,
                            error
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
                        && self.rom_checks.values().all(|check| check.valid)
                        && self.has_screen();

                    if ui
                        .add_enabled(can_proceed, egui::Button::new("Load and run"))
                        .clicked()
                    {
                        if !matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD) {
                            self.init_args.pmmu_enabled = false;
                        }
                        if !matches!(
                            self.selected_model,
                            MacModel::LC | MacModel::LCII | MacModel::LCIII | MacModel::LC475
                        ) {
                            self.init_args.fpu_enabled = false;
                        }

                        self.result = Some(ModelSelectionResult {
                            model: self.selected_model,
                            main_rom_path: PathBuf::from(&self.main_rom_path),
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
                                // Deprecated
                                mouse_disabled: None,
                                ..self.init_args.clone()
                            },
                            devices: self.result_devices(),
                        });
                        self.open = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.open = false;
                    }
                },
            );
        });

        if self.iie_settings_open {
            self.iie_settings_modal(ctx);
        }

        if last_model != self.selected_model {
            self.do_model_changed();
        }

        if last_validation_disabled != self.disable_rom_validation {
            self.do_validate_roms();
        }
    }

    /// The row where the video card of a NuBus slot is chosen, among the cards that fit
    /// Whether the Mac model has something to set in "Select peripherals"
    fn has_peripherals(&self) -> bool {
        let mac = self.selected_model;
        let display_choice = mac.buses().iter().any(|bus| {
            bus.kind == BusKind::BuiltIn
                && bus.slots.iter().any(|slot| {
                    slot.device.as_ref().is_some_and(|device| {
                        matches!(device.model.info().function, FunctionInfo::Display(info)
                            if info.monitors.len() > 1 || info.vram_sizes.len() > 1)
                    })
                })
        });
        let slots = mac.buses().iter().any(|bus| {
            matches!(bus.kind, BusKind::Nubus | BusKind::LcPds | BusKind::Se30Pds)
                && models_for(bus.kind, mac).next().is_some()
        });
        display_choice
            || slots
            || matches!(
                mac,
                MacModel::LC
                    | MacModel::LCII
                    | MacModel::LCIII
                    | MacModel::LC475
                    | MacModel::MacII
                    | MacModel::MacIIFDHD
            )
    }

    /// The rows of "Select peripherals": the LC 475's CPU, the monitor and the video RAM of
    /// the built-in video, the NuBus slots, the processor-direct slot, then the FPU and the
    /// PMMU of the Macs where they are a choice
    fn peripheral_rows(&mut self, ui: &mut egui::Ui, settings: &AppSettings) {
        let buses = self.selected_model.buses();
        if self.selected_model == MacModel::LC475 {
            // The CPU is socketed, and some owners fit a 68040
            ui.label("CPU:");
            egui::ComboBox::new(egui::Id::new("lc475_cpu"), "")
                .selected_text(if self.init_args.fpu_enabled {
                    "68040"
                } else {
                    "68LC040"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.init_args.fpu_enabled, false, "68LC040");
                    ui.selectable_value(&mut self.init_args.fpu_enabled, true, "68040");
                });
            ui.end_row();
        }

        // The monitor and the video RAM of the built-in video
        for bus in buses.iter().filter(|bus| bus.kind == BusKind::BuiltIn) {
            for slot in &bus.slots {
                self.display_rows(ui, &bus.id, &slot.id);
            }
        }

        // Each NuBus slot, with the ROM file, the monitor and the video RAM of its card
        for bus in buses.iter().filter(|bus| bus.kind == BusKind::Nubus) {
            for slot in &bus.slots {
                self.card_row(ui, bus, &slot.id);
                self.rom_row(ui, settings, &bus.id, &slot.id);
                self.display_rows(ui, &bus.id, &slot.id);
            }
        }
        if self.screens() > 1 {
            ui.label("");
            ui.label(
                egui::RichText::new(format!(
                    "{} More than one display is an experimental feature known to \
                     have issues",
                    egui_material_icons::icons::ICON_WARNING
                ))
                .color(egui::Color32::RED),
            );
            ui.end_row();
        }

        // The processor-direct slot, where a device model fits it
        let pds = buses.iter().find(|bus| {
            matches!(bus.kind, BusKind::LcPds | BusKind::Se30Pds)
                && models_for(bus.kind, self.selected_model).next().is_some()
        });
        if let Some(bus) = pds {
            self.pds_slot_row(ui, bus);
        }

        if matches!(
            self.selected_model,
            MacModel::LC | MacModel::LCII | MacModel::LCIII
        ) {
            ui.checkbox(&mut self.init_args.fpu_enabled, "Enable 68882 FPU");
            // The LC and LC II have no FPU socket on the board, a 68882
            // can only sit on a card in their one PDS slot
            let card =
                pds.is_some_and(|bus| self.config_index(&bus.id, &bus.slots[0].id).is_some());
            if self.init_args.fpu_enabled
                && card
                && matches!(self.selected_model, MacModel::LC | MacModel::LCII)
            {
                ui.label(
                    egui::RichText::new(format!(
                        "{} A real Mac LC cannot have a 68882 with this card installed",
                        egui_material_icons::icons::ICON_WARNING
                    ))
                    .color(ui.visuals().warn_fg_color),
                );
            }
            ui.end_row();
        }
        if matches!(self.selected_model, MacModel::MacII | MacModel::MacIIFDHD) {
            ui.checkbox(&mut self.init_args.pmmu_enabled, "Enable 68851 PMMU");
            ui.end_row();
        }
    }

    /// The row where the card of a NuBus slot is chosen, among the cards that fit
    fn card_row(&mut self, ui: &mut egui::Ui, bus: &Bus, slot: &str) {
        let current = self
            .config_index(&bus.id, slot)
            .and_then(|index| self.devices[index].model);
        let mut choice = current;
        ui.label(format!("{} slot {}", bus.title, slot));
        egui::ComboBox::new(egui::Id::new(("Select card", slot)), "")
            .selected_text(current.map_or("Empty", |model| model.info().name))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut choice, None, "Empty");
                for info in models_for(bus.kind, self.selected_model) {
                    ui.selectable_value(&mut choice, Some(info.model), info.name);
                }
            });
        ui.label("");
        ui.end_row();
        if choice != current {
            self.choose(&bus.id, slot, choice);
        }
    }

    /// The row where the ROM file of the device in a slot is chosen, if its device model
    /// needs one. Returns whether there is such a row.
    fn rom_row(
        &mut self,
        ui: &mut egui::Ui,
        settings: &AppSettings,
        bus: &str,
        slot: &str,
    ) -> bool {
        let Some(index) = self.config_index(bus, slot) else {
            return false;
        };
        let model = self.devices[index].model.map(DeviceModel::info);
        let Some(rom) = model.and_then(|info| info.rom.as_ref()) else {
            return false;
        };
        let mut path = match &self.devices[index].rom {
            Some(path) => path.to_string_lossy().to_string(),
            None => String::new(),
        };

        ui.label(rom.label);
        let mut edited = false;
        ui.horizontal(|ui| {
            let response = ui.text_edit_singleline(&mut path);
            if response.changed() {
                self.devices[index].rom = (!path.is_empty()).then(|| PathBuf::from(&path));
            }
            edited = response.lost_focus();
            if ui.button("Browse...").clicked() {
                self.device_rom_slot = (bus.to_string(), slot.to_string());
                self.device_rom_dialog
                    .pick_file(settings.native_file_dialogs);
            }
        });
        if edited {
            self.do_validate_device_roms();
        }

        // Validation indicator
        if !path.is_empty() {
            let check = self.rom_checks.get(&(bus.to_string(), slot.to_string()));
            let (icon, color) = if check.is_some_and(|check| check.valid) {
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
        true
    }

    /// Whether the Mac model has a slot other than this one that takes a device with the
    /// Display function
    fn another_display_possible(&self, bus: &str, slot: &str) -> bool {
        let mac = self.selected_model;
        mac.buses().iter().any(|other| {
            let displays = models_for(other.kind, mac)
                .any(|info| matches!(info.function, FunctionInfo::Display(_)));
            displays && other.slots.iter().any(|s| other.id != bus || s.id != slot)
        })
    }

    /// The rows of the video in a slot: its monitor and its video RAM, where its device model
    /// leaves a choice
    fn display_rows(&mut self, ui: &mut egui::Ui, bus: &str, slot: &str) {
        let Some(index) = self.config_index(bus, slot) else {
            return;
        };
        // "No monitor" only where the Mac model can have a display in another slot
        let offer_none = self.another_display_possible(bus, slot);
        let config = &mut self.devices[index];
        let function = config.model.map(|model| model.info().function);
        let (Some(FunctionInfo::Display(info)), FunctionConfig::Display(display)) =
            (function, &mut config.function)
        else {
            return;
        };
        let DisplayInfo {
            monitors,
            vram_sizes,
            ..
        } = info;

        if monitors.len() > 1 {
            // A monitor that the video can't have is the default one
            let shown = display
                .monitor
                .filter(|monitor| monitors.contains(monitor))
                .or_else(|| info.default_monitor())
                .unwrap_or_default();
            let mut monitor = shown;
            ui.label("Monitor");
            egui::ComboBox::new(egui::Id::new(("Select monitor", bus, slot)), "")
                .selected_text(format!("{}", shown))
                .show_ui(ui, |ui| {
                    for &choice in monitors {
                        if choice != MacMonitor::NoMonitor || offer_none {
                            ui.selectable_value(&mut monitor, choice, choice.to_string());
                        }
                    }
                });
            ui.end_row();
            if monitor != shown {
                display.monitor = Some(monitor);
            }
        }

        if vram_sizes.len() > 1 {
            let shown = display
                .vram_size
                .filter(|size| vram_sizes.contains(size))
                .or_else(|| info.default_vram_size())
                .unwrap_or_default();
            ui.label("Video RAM");
            egui::ComboBox::new(egui::Id::new(("Select video RAM", bus, slot)), "")
                .selected_text(format_ram(shown))
                .show_ui(ui, |ui| {
                    for &sz in vram_sizes {
                        ui.selectable_value(&mut display.vram_size, Some(sz), format_ram(sz));
                    }
                });
            ui.end_row();
        }
    }

    /// The checkbox that gives a Mac with 400K drives the 800K drive that fits it
    fn drive_checkbox(&mut self, ui: &mut egui::Ui) {
        let drive = DeviceModel::Mac800KPwmDrive;
        let buses = self.selected_model.buses();
        let Some(bus) = buses.iter().find(|bus| bus.kind == BusKind::Swim) else {
            return;
        };
        if !models_for(bus.kind, self.selected_model).any(|info| info.model == drive) {
            return;
        }
        let model_in = |dialog: &Self, slot: &str| {
            dialog
                .config_index(&bus.id, slot)
                .and_then(|index| dialog.devices[index].model)
        };
        let current = bus
            .slots
            .iter()
            .all(|slot| model_in(self, &slot.id) == Some(drive));
        let mut checked = current;
        ui.checkbox(&mut checked, "Use 800K floppy drive on Macintosh 128K/512K");
        if checked != current {
            self.drive_800k = checked;
            self.set_800k_drives(checked);
        }
    }

    /// Puts the 800K drive in every slot of the floppy bus, or the drive the Mac model has
    /// there, on a Mac model that takes that drive
    fn set_800k_drives(&mut self, on: bool) {
        let drive = DeviceModel::Mac800KPwmDrive;
        let buses = self.selected_model.buses();
        let Some(bus) = buses.iter().find(|bus| bus.kind == BusKind::Swim) else {
            return;
        };
        if !models_for(bus.kind, self.selected_model).any(|info| info.model == drive) {
            return;
        }
        for slot in &bus.slots {
            // Back to the drive the Mac model has
            let own = slot.device.as_ref().map(|device| device.model);
            self.choose(&bus.id, &slot.id, if on { Some(drive) } else { own });
        }
    }

    /// The grid row choosing the card in the PDS slot
    fn pds_slot_row(&mut self, ui: &mut egui::Ui, bus: &Bus) {
        let slot = &bus.slots[0].id;
        let current = self
            .config_index(&bus.id, slot)
            .and_then(|index| self.devices[index].model);
        let mut choice = current;
        ui.label("PDS Slot");
        ui.horizontal(|ui| {
            egui::ComboBox::new(egui::Id::new("Select PDS card"), "")
                .selected_text(current.map_or("None", |model| model.info().name))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice, None, "None");
                    for info in models_for(bus.kind, self.selected_model) {
                        ui.selectable_value(&mut choice, Some(info.model), info.name);
                    }
                });
            let settings =
                current.is_some_and(|model| model.info().function == FunctionInfo::AppleIIeCard);
            if ui
                .add_enabled(settings, egui::Button::new("Settings..."))
                .clicked()
            {
                self.iie_settings_open = true;
            }
        });
        ui.end_row();
        if choice != current {
            self.choose(&bus.id, slot, choice);
        }
    }

    /// The Apple IIe Card settings, a dialog over the main one
    fn iie_settings_modal(&mut self, ctx: &egui::Context) {
        let init_args = &mut self.init_args;
        let open = &mut self.iie_settings_open;
        let settings = self.devices.iter_mut().find_map(|c| match &mut c.function {
            FunctionConfig::AppleIIeCard(settings) => Some(settings),
            _ => None,
        });
        // The card may have left the slot since the dialog was opened
        let Some(settings) = settings else {
            *open = false;
            return;
        };
        egui::Modal::new(egui::Id::new("Apple IIe Card settings")).show(ctx, |ui| {
            ui.style_mut().spacing.item_spacing = egui::Vec2::splat(4.0);
            ui.set_width(400.0);

            ui.heading("Apple IIe Card settings");
            ui.separator();

            ui.checkbox(
                &mut settings.security_jumper,
                "Lock the Option Panel (J17 jumper)",
            );

            ui.separator();
            ui.label("External drives (Y-cable), in daisy-chain order:");
            egui::Grid::new("iie_drives_grid").show(ui, |ui| {
                ui.label("First drive:");
                egui::ComboBox::new(egui::Id::new("IIe first drive"), "")
                    .selected_text(iie_disk_device_name(settings.disk_chain[0]))
                    .show_ui(ui, |ui| {
                        for device in [
                            IieDiskDevice::None,
                            IieDiskDevice::Apple525,
                            IieDiskDevice::UniDisk35,
                        ] {
                            ui.selectable_value(
                                &mut settings.disk_chain[0],
                                device,
                                iie_disk_device_name(device),
                            );
                        }
                    });
                ui.end_row();

                // The second drive plugs into the first
                if settings.disk_chain[0] == IieDiskDevice::None {
                    settings.disk_chain[1] = IieDiskDevice::None;
                }
                ui.label("Second drive:");
                ui.add_enabled_ui(settings.disk_chain[0] != IieDiskDevice::None, |ui| {
                    egui::ComboBox::new(egui::Id::new("IIe second drive"), "")
                        .selected_text(iie_disk_device_name(settings.disk_chain[1]))
                        .show_ui(ui, |ui| {
                            for device in [IieDiskDevice::None, IieDiskDevice::Apple525] {
                                ui.selectable_value(
                                    &mut settings.disk_chain[1],
                                    device,
                                    iie_disk_device_name(device),
                                );
                            }
                        });
                });
                ui.end_row();
            });

            ui.separator();
            ui.checkbox(
                &mut init_args.iie_joystick_emulation,
                "Enable controller emulation",
            );
            if init_args.iie_joystick_emulation {
                ui.label(
                    "The IIe has a joystick while a host game controller is connected or the \
                     joystick keys are on. Otherwise its game port is empty, so games that look \
                     for a joystick when they start use the keyboard: switch the keys on before \
                     starting a game that should use them. Switched off, the keys go back to \
                     the IIe and the joystick stays connected, centred, until the IIe restarts. \
                     By default I/J/L/M, keypad 8/4/6/2, \
                     the arrows or W/A/S/D move it; X, 2, Left Alt or keypad . is button 0, Z, 1, Left Ctrl or keypad 0 is button 1. \
                     Switch the joystick keys on and off with Scroll Lock, the toolbar or \
                     the Machine menu, where the keys can also be changed.",
                );
            } else {
                ui.label(
                    "The IIe has a joystick only while a host game controller is connected. \
                     Otherwise its game port is empty, so games that look for a joystick when \
                     they start use the keyboard.",
                );
            }
            ui.label("Command and Option also press buttons 0 and 1 (Open and Closed Apple).");

            ui.separator();
            egui::Sides::new().show(
                ui,
                |_ui| {},
                |ui| {
                    if ui.button("Close").clicked() {
                        *open = false;
                    }
                },
            );
        });
    }

    fn do_validate_roms(&mut self) {
        self.do_validate_device_roms();
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

    /// Checks the ROM file of each device whose device model needs one
    fn do_validate_device_roms(&mut self) {
        self.rom_checks.clear();
        for config in &self.devices {
            let Some(rom) = config.model.and_then(|model| model.info().rom.as_ref()) else {
                continue;
            };
            let check =
                Self::validate_device_rom(rom, config.rom.as_deref(), self.disable_rom_validation);
            let check = match check {
                Ok(valid) => RomCheck {
                    valid,
                    error: String::new(),
                },
                Err(e) => RomCheck {
                    valid: false,
                    error: e.to_string(),
                },
            };
            self.rom_checks
                .insert((config.bus.clone(), config.slot.clone()), check);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_core::devices::{CdromConfig, DisplayConfig, built_in, bus_id};

    #[test]
    fn every_device_model_gets_a_config_of_its_function() {
        for model in DeviceModel::iter() {
            let info = model.info();
            assert!(
                info.function.takes(&default_config(info)),
                "{} gets the Config of another function",
                info.name
            );
        }
    }

    /// A dialog on a Mac model, with the device configs of a machine
    fn dialog(model: MacModel, devices: &[DeviceConfig]) -> ModelSelectionDialog {
        let mut dialog = ModelSelectionDialog::default();
        dialog.prefill(
            Some(model),
            None,
            None,
            None,
            &EmulatorInitArgs::default(),
            devices,
        );
        dialog
    }

    /// The slots that have a device in the dialog, with its device model
    fn slots(dialog: &ModelSelectionDialog) -> Vec<(&str, &str, DeviceModel)> {
        dialog
            .devices
            .iter()
            .map(|c| (c.bus.as_str(), c.slot.as_str(), c.model.unwrap()))
            .collect()
    }

    fn display(monitor: Option<MacMonitor>, vram_size: Option<usize>) -> FunctionConfig {
        FunctionConfig::Display(DisplayConfig { monitor, vram_size })
    }

    fn config(bus: &str, slot: &str, model: Option<DeviceModel>) -> DeviceConfig {
        let function = match model {
            Some(model) => default_config(model.info()),
            None => display(None, None),
        };
        DeviceConfig {
            bus: bus.into(),
            slot: slot.into(),
            model,
            rom: None,
            function,
        }
    }

    #[test]
    fn the_dialog_describes_the_devices_that_are_part_of_the_machine() {
        // An LC: its two drives and its video, and not what is on its SCSI bus
        let cdrom = DeviceConfig {
            function: FunctionConfig::Cdrom(CdromConfig::default()),
            ..config(bus_id::SCSI, "3", Some(DeviceModel::ScsiCdrom))
        };
        let dialog = dialog(MacModel::LC, &[cdrom]);
        assert_eq!(
            slots(&dialog),
            [
                (bus_id::SWIM, "1", DeviceModel::MacSuperDrive),
                (bus_id::SWIM, "2", DeviceModel::MacSuperDrive),
                (bus_id::BUILT_IN, built_in::VIDEO, DeviceModel::V8Video),
            ]
        );
        // With nothing chosen, the machine is the Mac model's own
        assert_eq!(dialog.result_devices(), []);
        assert!(dialog.has_screen());
    }

    #[test]
    fn a_macintosh_ii_gets_a_video_card_and_needs_a_screen() {
        let mut dialog = dialog(MacModel::MacII, &[]);
        let card = config(bus_id::NUBUS, "9", Some(DeviceModel::DisplayCard824));
        assert_eq!(dialog.result_devices(), [card]);
        assert!(dialog.has_screen());

        // The card needs its ROM file
        dialog.do_validate_device_roms();
        assert!(dialog.rom_checks.values().all(|check| !check.valid));

        // With nothing on the connector of its only card, the machine has no screen
        let index = dialog.config_index(bus_id::NUBUS, "9").unwrap();
        dialog.devices[index].function = display(Some(MacMonitor::NoMonitor), None);
        assert!(!dialog.has_screen());

        // A second card with a monitor is a second screen
        dialog.devices[index].function = display(None, None);
        dialog.choose(bus_id::NUBUS, "B", Some(DeviceModel::DisplayCard824));
        assert_eq!(dialog.screens(), 2);
        // The cards stay where they are when the dialog fits the list again
        dialog.fit_devices();
        let cards: Vec<_> = dialog
            .result_devices()
            .into_iter()
            .map(|c| c.slot)
            .collect();
        assert_eq!(cards, ["9", "B"]);
        dialog.choose(bus_id::NUBUS, "B", None);
        // And a machine whose only card isn't in the first slot gets no other
        dialog.choose(bus_id::NUBUS, "9", None);
        dialog.choose(bus_id::NUBUS, "C", Some(DeviceModel::DisplayCard824));
        dialog.fit_devices();
        let cards: Vec<_> = dialog
            .result_devices()
            .into_iter()
            .map(|c| c.slot)
            .collect();
        assert_eq!(cards, ["C"]);
        dialog.choose(bus_id::NUBUS, "C", None);
        dialog.fit_devices();
        let index = dialog.config_index(bus_id::NUBUS, "9").unwrap();
        dialog.devices[index].function = display(Some(MacMonitor::NoMonitor), None);

        // Another card takes its place, with the ROM file last used for it
        let rom = PathBuf::from("toby.rom");
        dialog
            .device_roms
            .push((DeviceModel::MacIIVideoCard, rom.clone()));
        dialog.choose(bus_id::NUBUS, "9", Some(DeviceModel::MacIIVideoCard));
        let toby = DeviceConfig {
            rom: Some(rom),
            ..config(bus_id::NUBUS, "9", Some(DeviceModel::MacIIVideoCard))
        };
        assert_eq!(dialog.result_devices(), [toby]);
        assert!(dialog.has_screen());
    }

    #[test]
    fn a_quadra_700_takes_a_card_next_to_its_video() {
        // Its slots are empty unless a card is chosen
        let mut dialog = dialog(MacModel::Quadra700, &[]);
        assert_eq!(dialog.result_devices(), []);
        assert_eq!(dialog.screens(), 1);

        dialog.choose(bus_id::NUBUS, "D", Some(DeviceModel::DisplayCard824));
        assert_eq!(dialog.screens(), 2);
        // With no monitor on its own video, the card's is its only screen
        let index = dialog
            .config_index(bus_id::BUILT_IN, built_in::VIDEO)
            .unwrap();
        dialog.devices[index].function = display(Some(MacMonitor::NoMonitor), None);
        assert_eq!(dialog.screens(), 1);
        let slots: Vec<_> = dialog
            .result_devices()
            .into_iter()
            .map(|c| c.slot)
            .collect();
        assert_eq!(slots, [built_in::VIDEO, "D"]);
    }

    #[test]
    fn a_change_of_mac_model_keeps_what_fits() {
        let video = DeviceConfig {
            function: display(Some(MacMonitor::RGB12), Some(256 * 1024)),
            ..config(bus_id::BUILT_IN, built_in::VIDEO, None)
        };
        let card = config(bus_id::PDS_LC, "0", Some(DeviceModel::AppleIIePdsCard));
        let mut dialog = dialog(MacModel::LC, &[video.clone(), card.clone()]);
        let lc_video = DeviceConfig {
            model: Some(DeviceModel::V8Video),
            ..video
        };
        assert_eq!(dialog.result_devices(), [lc_video, card.clone()]);

        // An LC II has the same video and the same slot. The video RAM goes back to the
        // usual size.
        dialog.selected_model = MacModel::LCII;
        dialog.do_model_changed();
        let lc_ii_video = DeviceConfig {
            function: display(Some(MacMonitor::RGB12), None),
            ..config(
                bus_id::BUILT_IN,
                built_in::VIDEO,
                Some(DeviceModel::V8Video),
            )
        };
        assert_eq!(dialog.result_devices(), [lc_ii_video, card.clone()]);

        // An LC III has another video, which keeps the monitor, and the same slot
        dialog.selected_model = MacModel::LCIII;
        dialog.do_model_changed();
        let lc_iii_video = DeviceConfig {
            function: display(Some(MacMonitor::RGB12), None),
            ..config(
                bus_id::BUILT_IN,
                built_in::VIDEO,
                Some(DeviceModel::SonoraVideo),
            )
        };
        assert_eq!(dialog.result_devices(), [lc_iii_video, card]);

        // A Plus has neither
        dialog.selected_model = MacModel::Plus;
        dialog.do_model_changed();
        assert_eq!(dialog.result_devices(), []);

        // The monitor is still noted for the next Mac model with a video
        dialog.selected_model = MacModel::LC;
        dialog.do_model_changed();
        let lc_video = DeviceConfig {
            function: display(Some(MacMonitor::RGB12), None),
            ..config(
                bus_id::BUILT_IN,
                built_in::VIDEO,
                Some(DeviceModel::V8Video),
            )
        };
        assert_eq!(dialog.result_devices(), [lc_video]);
    }

    #[test]
    fn no_monitor_is_offered_where_another_display_is_possible() {
        let offered = |mac, bus, slot| dialog(mac, &[]).another_display_possible(bus, slot);
        assert!(!offered(MacModel::LC, bus_id::BUILT_IN, built_in::VIDEO));
        assert!(!offered(MacModel::LCIII, bus_id::BUILT_IN, built_in::VIDEO));
        assert!(offered(MacModel::MacII, bus_id::NUBUS, "9"));
    }

    #[test]
    fn a_mac_with_400k_drives_takes_the_800k_drive() {
        let mut dialog = dialog(MacModel::Early128K, &[]);
        let drive = DeviceModel::Mac800KPwmDrive;
        dialog.choose(bus_id::SWIM, "1", Some(drive));
        dialog.choose(bus_id::SWIM, "2", Some(drive));
        assert_eq!(
            dialog.result_devices(),
            [
                config(bus_id::SWIM, "1", Some(drive)),
                config(bus_id::SWIM, "2", Some(drive)),
            ]
        );

        // A Plus doesn't
        dialog.selected_model = MacModel::Plus;
        dialog.do_model_changed();
        assert_eq!(dialog.result_devices(), []);

        // The choice is kept for the next Mac model that takes the drive
        dialog.selected_model = MacModel::Early512K;
        dialog.do_model_changed();
        assert_eq!(
            dialog.result_devices(),
            [
                config(bus_id::SWIM, "1", Some(drive)),
                config(bus_id::SWIM, "2", Some(drive)),
            ]
        );
    }

    #[test]
    fn the_rom_file_of_a_device_is_checked() {
        let rom = DeviceModel::DisplayCard824.info().rom.as_ref().unwrap();
        let check = ModelSelectionDialog::validate_device_rom;
        // No file: the device can't be made
        assert!(!check(rom, None, false).unwrap());

        let dir = std::env::temp_dir().join(format!("snow_modelselect_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("card.rom");
        fs::write(&file, [0; 16]).unwrap();
        // Not the ROM that is known to work
        let error = check(rom, Some(&file), false).err().unwrap().to_string();
        assert_eq!(error, "Invalid Display Card 8-24 ROM (341-0868).");
        // Any file that can be read, when the check is off
        assert!(check(rom, Some(&file), true).unwrap());
        assert!(check(rom, Some(&dir.join("none")), true).is_err());

        // A ROM with a serial number in bit 7 of each byte, which the digest leaves out
        let rom = RomInfo {
            label: "Test ROM",
            digests: &["374708fff7719dd5979ec875d56cd2286f6d3cf7ec317a3b25632aab28ec37bb"],
            optional: false,
            varying_bits: 0x80,
        };
        fs::write(&file, [0x80; 16]).unwrap();
        assert!(check(&rom, Some(&file), false).unwrap());
        fs::write(&file, [0x81; 16]).unwrap();
        assert!(check(&rom, Some(&file), false).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
