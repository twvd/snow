use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::dialogs::filedialog::SnowFileDialog;
use crate::emulator::EmulatorInitArgs;
use crate::settings::AppSettings;
use crate::workspace::VideoCardEntry;
use anyhow::{Result, anyhow, bail};
use eframe::egui;
use snow_core::cpu_m68k::M68040;
use snow_core::emulator::MouseMode;
use snow_core::emulator::devices::{BusKind, DeviceModel, models_for};
use snow_core::mac::maclc::lc_475_bus;
use snow_core::mac::maclc::pds_iie::IieDiskDevice;
use snow_core::mac::maclc::video as lc_video;
use snow_core::mac::quadra::dafb;
use snow_core::mac::quadra::q800_bus;
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{MacModel, MacMonitor, NubusDeviceKind, PdsCardKind};
use strum::IntoEnumIterator;

/// The device model of a card kind the machine is built with
const fn card_model(kind: NubusDeviceKind) -> DeviceModel {
    match kind {
        NubusDeviceKind::Toby => DeviceModel::MacIIVideoCard,
        NubusDeviceKind::SE30Video => DeviceModel::SE30Video,
        NubusDeviceKind::Mdc12 => DeviceModel::DisplayCard824,
    }
}

/// And back
const fn card_kind(model: DeviceModel) -> NubusDeviceKind {
    match model {
        DeviceModel::MacIIVideoCard => NubusDeviceKind::Toby,
        DeviceModel::SE30Video => NubusDeviceKind::SE30Video,
        _ => NubusDeviceKind::Mdc12,
    }
}

/// A video card in a NuBus slot, or the SE/30's video, as the dialog has it
struct SlotCard {
    kind: NubusDeviceKind,
    rom_path: String,
    rom_valid: bool,
    rom_error: String,
    /// What's on its connector, among the card's monitors
    monitor: Option<MacMonitor>,
}

impl SlotCard {
    /// Whether the card shows a screen: a video card with a monitor
    fn has_display(&self) -> bool {
        self.kind != NubusDeviceKind::Mdc12 || self.monitor.is_some()
    }

    fn new(kind: NubusDeviceKind, rom_path: String) -> Self {
        let mut card = Self {
            kind,
            rom_path,
            rom_valid: false,
            rom_error: String::new(),
            monitor: Some(MacMonitor::default()),
        };
        card.fit_monitor();
        card
    }

    /// The monitors the card can have
    fn monitors(&self) -> &'static [Option<MacMonitor>] {
        card_model(self.kind).info().monitors
    }

    /// A monitor the card can't have becomes its first one
    fn fit_monitor(&mut self) {
        let monitors = self.monitors();
        if !monitors.contains(&self.monitor) {
            self.monitor = monitors.first().copied().flatten();
        }
    }

    /// What the card's record calls its ROM
    fn rom_description(&self) -> &'static str {
        card_model(self.kind)
            .info()
            .rom
            .as_ref()
            .map_or("Video ROM", |rom| rom.label)
    }
}

/// How the dialog names a monitor choice
fn monitor_name(monitor: Option<MacMonitor>) -> String {
    monitor.map_or_else(|| "None".to_owned(), |m| m.to_string())
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
    roms_by_card: Vec<(NubusDeviceKind, PathBuf)>,
    selected_model: MacModel,
    init_args: EmulatorInitArgs,
    selected_monitor: MacMonitor,
    early_800k: bool,

    // Main ROM selection
    main_rom_path: String,
    main_rom_valid: bool,
    main_rom_dialog: SnowFileDialog,

    // Video cards by NuBus slot, the SE/30's video in its slot $E
    cards: BTreeMap<u8, SlotCard>,
    // The card whose ROM the file dialog picks
    rom_dialog_slot: u8,
    display_rom_dialog: SnowFileDialog,
    display_rom_required: bool,
    // Last ROM of each kind of card, for a card put in a slot
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
    pub video_cards: Vec<VideoCardEntry>,
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

            cards: BTreeMap::new(),
            rom_dialog_slot: 0,
            display_rom_dialog: SnowFileDialog::new()
                .add_filter("ROM files", &["rom", "bin", "uk6"])
                .show_pinned_folders(false)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir),
            display_rom_required: false,
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
        roms_by_card: Vec<(NubusDeviceKind, PathBuf)>,
    ) {
        self.open = true;
        self.iie_settings_open = false;
        self.last_roms = last_roms;
        self.roms_by_card = roms_by_card;
        self.main_rom_error.clear();
        self.result = None;

        self.do_model_changed();
    }

    /// Overrides the dialog's current values before opening
    #[allow(clippy::too_many_arguments)]
    pub fn prefill(
        &mut self,
        model: Option<MacModel>,
        main_rom_path: Option<&Path>,
        video_cards: &[VideoCardEntry],
        pram_path: Option<&Path>,
        extension_rom_path: Option<&Path>,
        init_args: &EmulatorInitArgs,
    ) {
        if let Some(model) = model {
            self.selected_model = model;
        }
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
        if !video_cards.is_empty() {
            self.cards.clear();
        }
        for entry in video_cards {
            let rom_path = entry.rom.to_string_lossy().to_string();
            self.card_rom_paths.insert(entry.kind, rom_path.clone());
            let mut card = SlotCard::new(entry.kind, rom_path);
            card.monitor = entry.monitor;
            card.fit_monitor();
            self.cards.insert(entry.slot, card);
        }
        self.fit_cards();
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
        self.update_display_rom_requirement();

        self.init_args.ram_size = None;
        self.init_args.vram_size = None;

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
        self.fit_cards();
        self.do_validate_roms();
    }

    /// The SE/30 has its video, a Mac II gets a card in its first slot, which the user can
    /// move or take out, and a machine with built-in video keeps the cards its slots take
    fn fit_cards(&mut self) {
        let slots = self.selected_model.nubus_slots();
        if self.selected_model == MacModel::SE30 {
            self.cards
                .retain(|_, c| c.kind == NubusDeviceKind::SE30Video);
            if self.cards.is_empty() {
                self.put_card(0xE, NubusDeviceKind::SE30Video);
            }
        } else if self.display_rom_required {
            self.cards
                .retain(|slot, c| slots.contains(slot) && c.kind != NubusDeviceKind::SE30Video);
            if self.cards.is_empty()
                && let Some(&slot) = slots.first()
            {
                self.put_card(slot, NubusDeviceKind::default());
            }
        } else if self.takes_cards() {
            self.cards
                .retain(|slot, c| slots.contains(slot) && c.kind != NubusDeviceKind::SE30Video);
        } else {
            self.cards.clear();
        }
    }

    /// Puts a card in a slot, with the last ROM of its kind
    fn put_card(&mut self, slot: u8, kind: NubusDeviceKind) {
        let rom_path = self.card_rom_paths.get(&kind).cloned().unwrap_or_default();
        self.cards.insert(slot, SlotCard::new(kind, rom_path));
        self.do_validate_card(slot);
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
        if !self.display_rom_required && !self.takes_cards() {
            self.cards.clear();
        }
    }

    /// Whether the machine has settings under "Select peripherals": a monitor, cards or
    /// options
    fn has_peripherals(&self) -> bool {
        self.monitor_selectable()
            || matches!(
                self.selected_model,
                MacModel::MacII
                    | MacModel::MacIIFDHD
                    | MacModel::MacIIx
                    | MacModel::MacIIcx
                    | MacModel::LC
                    | MacModel::LCII
                    | MacModel::LCIII
            )
    }

    /// Whether the machine has built-in video and NuBus slots that take video
    /// cards beside it
    fn takes_cards(&self) -> bool {
        matches!(self.selected_model, MacModel::Quadra700)
    }

    /// The monitor of the built-in video. A card's is in its slot's rows.
    fn monitor_selectable(&self) -> bool {
        matches!(
            self.selected_model,
            MacModel::Quadra700 | MacModel::LC475 | MacModel::Quadra800
        )
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

    fn validate_card_rom(&self, card: &SlotCard) -> Result<()> {
        // Skip validation if disabled
        if self.disable_rom_validation {
            // Just check if the file exists and is readable
            std::fs::read(&card.rom_path)
                .map_err(|e| anyhow!("Cannot read {}: {}", card.rom_description(), e))?;
            return Ok(());
        }

        // The card's record knows the ROMs it works with
        let rom = std::fs::read(&card.rom_path)
            .map_err(|e| anyhow!("Invalid {}: {}", card.rom_description(), e))?;
        if card_model(card.kind).info().validate_rom(&rom) {
            Ok(())
        } else {
            bail!("Invalid {}.", card.rom_description())
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

        if let Some(path) = self.display_rom_dialog.take_picked()
            && let Some(card) = self.cards.get_mut(&self.rom_dialog_slot)
        {
            card.rom_path = path.to_string_lossy().to_string();
            self.card_rom_paths.insert(card.kind, card.rom_path.clone());
            self.do_validate_card(self.rom_dialog_slot);
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

                // The SE/30's video is on the logic board: only its ROM is asked for
                if self.selected_model == MacModel::SE30 {
                    self.video_rom_row(ui, settings, 0xE);
                }
                if self.has_peripherals() {
                    ui.label(egui::RichText::from("Select peripherals").strong());
                    ui.end_row();
                }

                // Monitors dropdown
                if self.monitor_selectable() {
                    let monitors = match self.selected_model {
                        MacModel::LC475 | MacModel::Quadra800 => dafb::MONITORS_MEMCJR.to_vec(),
                        _ => dafb::MONITORS.to_vec(),
                    };
                    if !monitors.contains(&self.selected_monitor) {
                        self.selected_monitor = MacMonitor::default();
                    }
                    ui.label("Monitor");
                    egui::ComboBox::new(egui::Id::new("Select monitor"), "")
                        .selected_text(format!("{}", self.selected_monitor))
                        .show_ui(ui, |ui| {
                            for monitor in monitors {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    monitor,
                                    monitor.to_string(),
                                );
                            }
                        });
                    ui.end_row();
                }
                if matches!(
                    self.selected_model,
                    MacModel::LC | MacModel::LCII | MacModel::LCIII
                ) {
                    if !lc_video::monitors(self.selected_model).contains(&self.selected_monitor) {
                        self.selected_monitor = MacMonitor::default();
                    }
                    ui.label("Monitor");
                    egui::ComboBox::new(egui::Id::new("Select LC monitor"), "")
                        .selected_text(format!("{}", self.selected_monitor))
                        .show_ui(ui, |ui| {
                            for &monitor in lc_video::monitors(self.selected_model) {
                                ui.selectable_value(
                                    &mut self.selected_monitor,
                                    monitor,
                                    monitor.to_string(),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Video RAM");
                    egui::ComboBox::new(egui::Id::new("Select LC video RAM"), "")
                        .selected_text(format_ram(
                            self.init_args.vram_size.unwrap_or_else(|| {
                                lc_video::vram_size_default(self.selected_model)
                            }),
                        ))
                        .show_ui(ui, |ui| {
                            for &sz in lc_video::vram_sizes(self.selected_model) {
                                ui.selectable_value(
                                    &mut self.init_args.vram_size,
                                    Some(sz),
                                    format_ram(sz),
                                );
                            }
                        });
                    ui.end_row();

                    ui.checkbox(&mut self.init_args.fpu_enabled, "Enable 68882 FPU");
                    // The LC and LC II have no FPU socket on the board, a 68882
                    // can only sit on a card in their one PDS slot
                    if self.init_args.fpu_enabled
                        && self.init_args.pds_card != PdsCardKind::None
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

                    self.pds_slot_row(ui);
                }
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

                    ui.label("VRAM size:");
                    egui::ComboBox::new(egui::Id::new("lc475_vram"), "")
                        .selected_text(format_ram(self.init_args.vram_size.unwrap_or(1024 * 1024)))
                        .show_ui(ui, |ui| {
                            for &sz in lc_475_bus::VRAM_SIZES {
                                ui.selectable_value(
                                    &mut self.init_args.vram_size,
                                    Some(sz),
                                    format_ram(sz),
                                );
                            }
                        });
                    ui.end_row();

                    self.pds_slot_row(ui);
                }
                if self.selected_model == MacModel::Quadra800 {
                    ui.label("VRAM size:");
                    egui::ComboBox::new(egui::Id::new("q800_vram"), "")
                        .selected_text(format_ram(self.init_args.vram_size.unwrap_or(1024 * 1024)))
                        .show_ui(ui, |ui| {
                            for &sz in q800_bus::VRAM_SIZES {
                                ui.selectable_value(
                                    &mut self.init_args.vram_size,
                                    Some(sz),
                                    format_ram(sz),
                                );
                            }
                        });
                    ui.end_row();
                }
                // A row per NuBus slot, with the card's own rows under it
                if (self.display_rom_required && self.selected_model != MacModel::SE30)
                    || self.takes_cards()
                {
                    self.nubus_rows(ui, settings);
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
                        }
                        if self.selected_model == MacModel::SE30 {
                            let mut x = false;
                            ui.checkbox(&mut x, "Emulate exploded PRAM battery");
                        }
                    });
                });
            });

            // Error messages
            let card_errors = self
                .cards
                .values()
                .filter(|c| !c.rom_error.is_empty())
                .map(|c| c.rom_error.clone())
                .collect::<Vec<_>>();
            if !self.main_rom_error.is_empty() || !card_errors.is_empty() {
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
                for error in card_errors {
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
                        && (!self.display_rom_required || !self.cards.is_empty())
                        && self.cards.values().all(|c| c.rom_valid);

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
                            self.init_args.pds_card = PdsCardKind::None;
                            if self.selected_model != MacModel::Quadra800 {
                                self.init_args.vram_size = None;
                            }
                        }

                        self.result = Some(ModelSelectionResult {
                            model: self.selected_model,
                            main_rom_path: PathBuf::from(&self.main_rom_path),
                            video_cards: self
                                .cards
                                .iter()
                                .map(|(&slot, card)| VideoCardEntry {
                                    slot,
                                    kind: card.kind,
                                    rom: PathBuf::from(&card.rom_path),
                                    monitor: card.monitor,
                                })
                                .collect(),
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
                                monitor: if self.monitor_selectable()
                                    || matches!(
                                        self.selected_model,
                                        MacModel::LC | MacModel::LCII | MacModel::LCIII
                                    ) {
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

    /// The video ROM, under the card it belongs to
    /// A row per NuBus slot, with the card's own rows under it
    fn nubus_rows(&mut self, ui: &mut egui::Ui, settings: &AppSettings) {
        let mac = self.selected_model;
        for &slot in mac.nubus_slots() {
            let current = self.cards.get(&slot).map(|c| card_model(c.kind));
            ui.label(format!("NuBus slot {:X}", slot));
            let mut choice = current;
            egui::ComboBox::new(egui::Id::new(("nubus slot", slot)), "")
                .selected_text(current.map_or("Empty", |m| m.name()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice, None, "Empty");
                    for info in models_for(BusKind::Nubus, mac) {
                        ui.selectable_value(&mut choice, Some(info.model), info.name);
                    }
                });
            ui.label("");
            ui.end_row();
            if choice != current {
                match choice {
                    Some(model) => self.put_card(slot, card_kind(model)),
                    None => {
                        self.cards.remove(&slot);
                    }
                }
            }
            if self.cards.contains_key(&slot) {
                self.video_rom_row(ui, settings, slot);
                self.card_monitor_row(ui, slot);
            }
        }
        if self.display_rom_required && self.cards.is_empty() {
            ui.label("");
            ui.label(
                egui::RichText::new(format!(
                    "{} This Macintosh needs a video card",
                    egui_material_icons::icons::ICON_WARNING
                ))
                .color(ui.visuals().warn_fg_color),
            );
            ui.end_row();
        }
        // The built-in video is a display too
        let displays = self.cards.values().filter(|c| c.has_display()).count()
            + usize::from(self.takes_cards());
        if displays > 1 {
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
    }

    fn video_rom_row(&mut self, ui: &mut egui::Ui, settings: &AppSettings, slot: u8) {
        let Some(card) = self.cards.get_mut(&slot) else {
            return;
        };
        ui.label(format!("    {}", card.rom_description()));
        let mut edited = false;
        ui.horizontal(|ui| {
            edited = ui.text_edit_singleline(&mut card.rom_path).lost_focus();
            if ui.button("Browse...").clicked() {
                self.rom_dialog_slot = slot;
                self.display_rom_dialog
                    .pick_file(settings.native_file_dialogs);
            }
        });
        if !card.rom_path.is_empty() {
            let (icon, color) = if card.rom_valid {
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
        if edited {
            self.card_rom_paths.insert(card.kind, card.rom_path.clone());
            self.do_validate_card(slot);
        }
    }

    /// What's on the card's connector, among what the card can have
    fn card_monitor_row(&mut self, ui: &mut egui::Ui, slot: u8) {
        let Some(card) = self.cards.get_mut(&slot) else {
            return;
        };
        let monitors = card.monitors();
        if monitors.is_empty() {
            return;
        }
        ui.label("    Monitor");
        egui::ComboBox::new(egui::Id::new(("card monitor", slot)), "")
            .selected_text(monitor_name(card.monitor))
            .show_ui(ui, |ui| {
                for &monitor in monitors {
                    ui.selectable_value(&mut card.monitor, monitor, monitor_name(monitor));
                }
            });
        ui.end_row();
    }

    /// The Apple IIe Card settings, a dialog over the main one
    /// The grid row choosing the card in the PDS slot
    fn pds_slot_row(&mut self, ui: &mut egui::Ui) {
        ui.label("PDS Slot");
        ui.horizontal(|ui| {
            egui::ComboBox::new(egui::Id::new("Select PDS card"), "")
                .selected_text(format!("{}", self.init_args.pds_card))
                .show_ui(ui, |ui| {
                    for card in PdsCardKind::iter() {
                        ui.selectable_value(&mut self.init_args.pds_card, card, card.to_string());
                    }
                });
            if ui
                .add_enabled(
                    self.init_args.pds_card == PdsCardKind::AppleIIe,
                    egui::Button::new("Settings..."),
                )
                .clicked()
            {
                self.iie_settings_open = true;
            }
        });
        ui.end_row();
    }

    fn iie_settings_modal(&mut self, ctx: &egui::Context) {
        egui::Modal::new(egui::Id::new("Apple IIe Card settings")).show(ctx, |ui| {
            ui.style_mut().spacing.item_spacing = egui::Vec2::splat(4.0);
            ui.set_width(400.0);

            ui.heading("Apple IIe Card settings");
            ui.separator();

            let settings = &mut self.init_args.iie_card_settings;
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
                &mut self.init_args.iie_joystick_emulation,
                "Enable controller emulation",
            );
            if self.init_args.iie_joystick_emulation {
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
                        self.iie_settings_open = false;
                    }
                },
            );
        });
    }

    fn do_validate_roms(&mut self) {
        let slots = self.cards.keys().copied().collect::<Vec<_>>();
        for slot in slots {
            self.do_validate_card(slot);
        }
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

    fn do_validate_card(&mut self, slot: u8) {
        let Some(card) = self.cards.get(&slot) else {
            return;
        };
        // No ROM yet: the row shows it
        let result = if card.rom_path.is_empty() {
            Err(None)
        } else {
            self.validate_card_rom(card).map_err(Some)
        };
        let card = self.cards.get_mut(&slot).unwrap();
        card.rom_valid = result.is_ok();
        card.rom_error = match result {
            Err(Some(e)) => format!("NuBus slot {:X}: {}", slot, e),
            _ => String::new(),
        };
    }
}
