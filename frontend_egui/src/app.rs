use crate::dialogs::about::AboutDialog;
use crate::dialogs::diskimage::{DiskImageDialog, DiskImageDialogResult};
use crate::dialogs::modelselect::{ModelSelectionDialog, ModelSelectionResult};
use crate::emulator::EmulatorState;
use crate::emulator::{EmulatorInitArgs, ScsiTargets};
use crate::keymap::map_winit_keycode;
use crate::settings::AppSettings;
use crate::uniform::{UniformAction, UNIFORM_ACTION};
use crate::widgets::breakpoints::BreakpointsWidget;
use crate::widgets::disassembly::DisassemblyWidget;
use crate::widgets::framebuffer::{FramebufferWidget, ScalingAlgorithm};
use crate::widgets::instruction_history::InstructionHistoryWidget;
use crate::widgets::memory::MemoryViewerWidget;
use crate::widgets::peripherals::PeripheralsWidget;
use crate::widgets::registers::RegistersWidget;
use crate::widgets::systrap_history::SystrapHistoryWidget;
use crate::widgets::terminal::TerminalWidget;
use crate::widgets::watchpoints::WatchpointsWidget;
use crate::workspace::{FramebufferMode, Workspace};
use snow_core::bus::Address;
use snow_core::emulator::comm::UserMessageType;
use snow_core::emulator::save::{load_state_header, SaveHeader};
use snow_core::mac::scc::SccCh;
use snow_core::mac::scsi::target::ScsiTargetType;
use snow_core::mac::serial_bridge::SerialBridgeConfig;
use snow_core::mac::MacModel;
use snow_floppy::loaders::{FloppyImageLoader, FloppyImageSaver, ImageType};
use snow_floppy::{Floppy, FloppyImage, FloppyType, OriginalTrackType};

use anyhow::{anyhow, bail, Context, Result};
use eframe::egui;
use egui_file_dialog::{DialogMode, DirectoryEntry, FileDialog};
use egui_toast::{Toast, ToastKind, ToastOptions};
use itertools::Itertools;
use rand::Rng;
use strum::IntoEnumIterator;

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs};

macro_rules! persistent_window_s {
    ($gui:expr, $title:expr, $default_size:expr) => {{
        let mut w = egui::Window::new($title);
        if let Some(r) = $gui.workspace.get_window($title) {
            w = w.default_rect(r);
        } else {
            w = w.default_size($default_size);
        }
        w
    }};
}

macro_rules! persistent_window {
    ($gui:expr, $title:expr) => {{
        let mut w = egui::Window::new($title);
        if let Some(r) = $gui.workspace.get_window($title) {
            w = w.default_rect(r);
        }
        w
    }};
}

fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        None => s,
        Some((idx, _)) => &s[..idx],
    }
}

enum FloppyDialogTarget {
    /// Insert disk into a drive / save an image from a drive to a file
    Drive(usize),
    /// Save this image to a file (invalid on load)
    Image(Box<FloppyImage>),
}

#[derive(Clone)]
struct Snowflake {
    x: f32,
    y: f32,
    velocity_x: f32,
    velocity_y: f32,
    size: f32,
    opacity: f32,
}

impl Snowflake {
    fn new(screen_width: f32) -> Self {
        let mut rng = rand::rng();
        Self {
            x: rng.random_range(0.0..screen_width),
            y: -10.0,
            velocity_x: rng.random_range(-1.0..1.0),
            velocity_y: rng.random_range(1.0..3.0),
            size: rng.random_range(2.0..6.0),
            opacity: rng.random_range(0.5..1.0),
        }
    }

    fn update(&mut self, delta_time: f32) {
        self.x += self.velocity_x * delta_time * 60.0;
        self.y += self.velocity_y * delta_time * 60.0;

        // Add some drift
        self.velocity_x += (rand::rng().random::<f32>() - 0.5) * 0.1 * delta_time;
        self.velocity_x = self.velocity_x.clamp(-2.0, 2.0);
    }

    fn is_off_screen(&self, screen_height: f32) -> bool {
        self.y > screen_height + 10.0
    }

    fn draw(&self, ui: &egui::Ui) {
        let rect = egui::Rect::from_center_size(
            egui::Pos2::new(self.x, self.y),
            egui::Vec2::splat(self.size),
        );

        ui.painter().rect_filled(
            rect,
            egui::Rounding::same(1.0),
            egui::Color32::from_white_alpha((self.opacity * 255.0) as u8),
        );
    }
}

pub struct SnowGui {
    workspace: Workspace,
    workspace_file: Option<PathBuf>,
    load_windows: bool,
    first_draw: bool,
    in_fullscreen: bool,
    in_zen_mode: bool,

    wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,

    toasts: egui_toast::Toasts,
    framebuffer: FramebufferWidget,
    registers: RegistersWidget,
    breakpoints: BreakpointsWidget,
    memory: MemoryViewerWidget,
    watchpoints: WatchpointsWidget,
    instruction_history: InstructionHistoryWidget,
    systrap_history: SystrapHistoryWidget,
    terminal: [TerminalWidget; 2],
    disassembly: DisassemblyWidget,

    workspace_dialog: FileDialog,
    hdd_dialog: FileDialog,
    hdd_dialog_idx: usize,
    cdrom_dialog: FileDialog,
    cdrom_dialog_idx: usize,
    cdrom_files_dialog: FileDialog,
    floppy_dialog: FileDialog,
    floppy_dialog_last: Option<DirectoryEntry>,
    floppy_dialog_last_image: Option<FloppyImage>,
    floppy_dialog_last_type: Option<ImageType>,
    floppy_dialog_target: FloppyDialogTarget,
    floppy_dialog_wp: bool,
    create_disk_dialog: DiskImageDialog,
    record_dialog: FileDialog,
    model_dialog: ModelSelectionDialog,
    about_dialog: AboutDialog,
    state_dialog: FileDialog,
    state_dialog_last: Option<DirectoryEntry>,
    state_dialog_last_header: Option<SaveHeader>,
    state_dialog_screenshot: egui::TextureHandle,
    shared_dir_dialog: FileDialog,

    error_dialog_open: bool,
    error_string: String,
    ui_active: bool,
    last_running: bool,

    // Snowflakes
    snowflakes: Vec<Snowflake>,
    last_snowflake_time: Instant,
    snowflake_spawn_timer: f32,

    settings: AppSettings,
    emu: EmulatorState,

    floppy_rpm_adjustment: [i32; 3],

    /// Temporary files that need cleanup on exit
    temp_files: Vec<PathBuf>,

    /// Quick state save slots
    quick_states: [Option<PathBuf>; 5],

    /// Pending serial bridge configurations from CLI args (applied after emulator starts)
    pending_serial_bridges: [Option<SerialBridgeConfig>; 2],

    /// Whether CLI serial bridges have been applied (to avoid re-applying on each frame)
    serial_bridges_applied: bool,
}

impl SnowGui {
    const TOAST_DURATION: Duration = Duration::from_secs(3);
    const ZOOM_FACTORS: [f32; 8] = [0.5, 0.8, 1.0, 1.2, 1.5, 2.0, 3.0, 4.0];
    const SUBMENU_WIDTH: f32 = 175.0;

    /// Parse serial bridge mode string from CLI argument
    fn parse_serial_bridge_mode(mode: &str) -> Option<SerialBridgeConfig> {
        let mode = mode.to_lowercase();
        if mode == "pty" {
            Some(SerialBridgeConfig::Pty)
        } else if let Some(port_str) = mode.strip_prefix("tcp:") {
            port_str.parse::<u16>().ok().map(SerialBridgeConfig::Tcp)
        } else {
            log::warn!(
                "Invalid serial bridge mode: '{}'. Use 'pty' or 'tcp:PORT'",
                mode
            );
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,
        initial_file: Option<String>,
        zoom_factor: f32,
        fullscreen: bool,
        zen: bool,
        serial_bridge_a: Option<&str>,
        serial_bridge_b: Option<&str>,
    ) -> Self {
        egui_material_icons::initialize(&cc.egui_ctx);
        cc.egui_ctx.set_zoom_factor(zoom_factor);

        let floppy_filter_str = format!(
            "Floppy images ({})",
            snow_floppy::loaders::ImageType::EXTENSIONS
                .into_iter()
                .map(|e| format!("*.{}", e.to_ascii_uppercase()))
                .join(", ")
        );
        let hdd_filter_str = "HDD images (*.img, *.hda)";
        let cdrom_filter_str = "CD-ROM images (*.iso, *.toast)";
        let settings = AppSettings::load();

        let mut app = Self {
            settings: settings.clone(),
            workspace: Default::default(),
            workspace_file: None,
            load_windows: false,
            first_draw: true,
            in_fullscreen: false,
            in_zen_mode: false,

            wev_recv,
            toasts: egui_toast::Toasts::new()
                .anchor(egui::Align2::CENTER_BOTTOM, (0.0, -30.0))
                .direction(egui::Direction::BottomUp),
            framebuffer: FramebufferWidget::new(cc),
            registers: RegistersWidget::new(),
            breakpoints: BreakpointsWidget::default(),
            memory: MemoryViewerWidget::default(),
            watchpoints: WatchpointsWidget::default(),
            instruction_history: InstructionHistoryWidget::default(),
            systrap_history: SystrapHistoryWidget::default(),
            terminal: Default::default(),
            disassembly: DisassemblyWidget::new(),

            hdd_dialog: FileDialog::new()
                .add_file_filter(
                    hdd_filter_str,
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("img")
                            || p.extension()
                                .unwrap_or_default()
                                .eq_ignore_ascii_case("hda")
                    }),
                )
                .default_file_filter(hdd_filter_str)
                .add_save_extension("Device image", "img")
                .default_save_extension("Device image")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_hdd),
            hdd_dialog_idx: 0,
            cdrom_dialog: FileDialog::new()
                .add_file_filter(
                    cdrom_filter_str,
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("iso")
                            || p.extension()
                                .unwrap_or_default()
                                .eq_ignore_ascii_case("toast")
                    }),
                )
                .default_file_filter(cdrom_filter_str)
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_cdrom),
            cdrom_dialog_idx: 0,
            cdrom_files_dialog: FileDialog::new()
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_cdrom_files),
            floppy_dialog: FileDialog::new()
                .add_file_filter(
                    &floppy_filter_str,
                    Arc::new(|p| {
                        let ext = p
                            .extension()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();

                        snow_floppy::loaders::ImageType::EXTENSIONS
                            .into_iter()
                            .any(|s| ext.eq_ignore_ascii_case(s))
                    }),
                )
                .default_file_filter(&floppy_filter_str)
                .add_save_extension("Applesauce MOOF", "moof")
                .default_save_extension("Applesauce MOOF")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_floppy),
            record_dialog: FileDialog::new()
                .allow_path_edit_to_save_file_without_extension(false)
                .add_save_extension("Snow recording", "snowr")
                .default_save_extension("Snow recording")
                .add_file_filter(
                    "Snow recording (*.snowr)",
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("snowr")
                    }),
                )
                .default_file_filter("Snow recording (*.snowr)")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_record),
            floppy_dialog_target: FloppyDialogTarget::Drive(0),
            floppy_dialog_last: None,
            floppy_dialog_last_image: None,
            floppy_dialog_last_type: None,
            floppy_dialog_wp: false,
            workspace_dialog: FileDialog::new()
                .add_file_filter(
                    "Snow workspace (*.snoww)",
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("snoww")
                    }),
                )
                .default_file_filter("Snow workspace (*.snoww)")
                .add_save_extension("Snow workspace", "snoww")
                .default_save_extension("Snow workspace")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_workspace),
            create_disk_dialog: Default::default(),
            model_dialog: Default::default(),
            about_dialog: AboutDialog::new(&cc.egui_ctx),
            state_dialog: FileDialog::new()
                .add_file_filter(
                    "Snow state file (*.snows)",
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("snows")
                    }),
                )
                .default_file_filter("Snow state file (*.snows)")
                .add_save_extension("Snow state file", "snows")
                .default_save_extension("Snow state file")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_state),
            state_dialog_last: None,
            state_dialog_last_header: None,
            state_dialog_screenshot: cc.egui_ctx.load_texture(
                "state_screenshot",
                egui::ColorImage::new([0, 0], egui::Color32::BLACK),
                egui::TextureOptions::LINEAR,
            ),
            shared_dir_dialog: FileDialog::new()
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir())
                .storage(settings.fd_shared_dir),

            error_dialog_open: false,
            error_string: String::new(),
            ui_active: true,
            last_running: false,

            // Snowflakes
            snowflakes: Vec::new(),
            last_snowflake_time: Instant::now(),
            snowflake_spawn_timer: 0.0,

            emu: EmulatorState::default(),

            floppy_rpm_adjustment: [0, 0, 0],

            // Always clean up images created by restoring save states
            temp_files: Vec::from_iter(
                (0..snow_core::mac::scsi::controller::ScsiController::MAX_TARGETS).map(|i| {
                    let mut pb = env::temp_dir();
                    pb.push(format!("snow_state_{}_{}.img", std::process::id(), i));
                    pb
                }),
            ),

            quick_states: Default::default(),

            pending_serial_bridges: [
                serial_bridge_a.and_then(Self::parse_serial_bridge_mode),
                serial_bridge_b.and_then(Self::parse_serial_bridge_mode),
            ],

            serial_bridges_applied: false,
        };

        if let Some(filename) = initial_file {
            let path = Path::new(&filename);
            if path
                .extension()
                .unwrap_or_default()
                .eq_ignore_ascii_case("snoww")
            {
                app.load_workspace(Some(path));
            } else if path
                .extension()
                .unwrap_or_default()
                .eq_ignore_ascii_case("rom")
            {
                app.load_rom_from_path(
                    path,
                    None,
                    None,
                    None,
                    None,
                    &EmulatorInitArgs::default(),
                    None,
                );
            }
            if fullscreen {
                app.enter_fullscreen(&cc.egui_ctx);
            } else if zen {
                app.enter_zen_mode();
            }
        }

        #[cfg(debug_assertions)]
        {
            app.toasts.add(Toast::default()
                .text("You are running a DEBUG BUILD of Snow which will be very, very SLOW!\n\nSee docs/BUILDING.md for instructions on building Snow in release mode")
                .options(ToastOptions::default())
                .kind(ToastKind::Warning));
        }

        app
    }

    fn enter_fullscreen(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        self.in_fullscreen = true;
        self.toasts.add(
            egui_toast::Toast::default()
                .text("RIGHT-CLICK to exit fullscreen or other actions")
                .options(
                    egui_toast::ToastOptions::default()
                        .duration(Self::TOAST_DURATION)
                        .show_progress(true),
                ),
        );
    }

    fn exit_fullscreen(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        self.in_fullscreen = false;
    }

    fn enter_zen_mode(&mut self) {
        self.in_zen_mode = true;
        self.toasts.add(
            egui_toast::Toast::default()
                .text("RIGHT-CLICK for actions")
                .options(
                    egui_toast::ToastOptions::default()
                        .duration(Self::TOAST_DURATION)
                        .show_progress(true),
                ),
        );
    }

    fn exit_zen_mode(&mut self) {
        self.in_zen_mode = false;
    }

    fn is_ui_hidden(&self) -> bool {
        self.in_fullscreen || self.in_zen_mode
    }

    fn try_create_image(&self, result: &DiskImageDialogResult) -> Result<()> {
        if result.filename.try_exists()? {
            bail!("Cowardly refusing to overwrite existing file. Delete the file first, or choose a different filename.");
        }

        {
            let mut file = File::create(result.filename.clone())?;
            file.seek(SeekFrom::Start(result.size as u64 - 1))?;
            file.write_all(&[0])?;
            file.flush()?;
        }
        self.emu.scsi_attach_hdd(result.scsi_id, &result.filename);
        Ok(())
    }

    fn draw_menubar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("Workspace", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);

                if ui.button("New workspace").clicked() {
                    self.load_workspace(None);
                    self.update_titlebar(ctx);
                    ui.close_menu();
                }
                if ui.button("Load workspace").clicked() {
                    self.workspace_dialog.pick_file();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Save workspace").clicked() {
                    if let Some(path) = self.workspace_file.clone() {
                        self.save_workspace(&path);
                    } else {
                        self.workspace_dialog.save_file();
                    }
                    ui.close_menu();
                }
                if ui.button("Save workspace as...").clicked() {
                    self.workspace_dialog.save_file();
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button("Recent workspaces", |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    for (i, path, display_name) in self.settings.get_recent_workspaces_for_display()
                    {
                        if ui.button(format!("{}: {}", i, display_name)).clicked() {
                            self.load_workspace(Some(&path));
                            self.update_titlebar(ctx);
                            ui.close_menu();
                        }
                    }
                    if self.settings.recent_workspaces.is_empty() {
                        ui.weak("No recent workspaces");
                    }
                });
                ui.separator();
                if ui.button("Exit").clicked() {
                    std::process::exit(0);
                }
            });
            ui.menu_button("Machine", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                if ui.button("Load ROM...").clicked() {
                    self.model_dialog.open(
                        self.settings.get_last_roms(),
                        self.settings.get_last_display_roms(),
                    );
                    ui.close_menu();
                }
                if self.emu.is_initialized() {
                    if ui.button("Reset").clicked() {
                        self.emu.reset();
                        ui.close_menu();
                    }

                    if self.emu.is_running() && ui.button("Stop").clicked() {
                        self.emu.stop();
                        ui.close_menu();
                    } else if !self.emu.is_running() && ui.button("Run").clicked() {
                        self.emu.run();
                        ui.close_menu();
                    }
                    if ui.button("Single step").clicked() {
                        self.emu.step();
                        ui.close_menu();
                    }
                    if ui.button("Step over").clicked() {
                        self.emu.step_over();
                        ui.close_menu();
                    }
                    if ui.button("Step out").clicked() {
                        self.emu.step_out();
                        ui.close_menu();
                    }

                    ui.separator();
                    if ui.button("Programmers key").clicked() {
                        self.emu.progkey();
                        ui.close_menu();
                    }
                }
            });

            ui.menu_button("State", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                if ui.button("Load state from file...").clicked() {
                    self.state_dialog.pick_file();
                    ui.close_menu();
                }
                if ui
                    .add_enabled(
                        self.emu.is_initialized(),
                        egui::Button::new("Save state to file..."),
                    )
                    .clicked()
                {
                    self.state_dialog.save_file();
                    ui.close_menu();
                }
                ui.separator();
                ui.strong("Quick load states");
                let mut load_file = None;
                for (i, p) in self.quick_states.iter().map(|p| p.as_ref()).enumerate() {
                    if ui
                        .add_enabled(
                            self.quick_states[i].is_some(),
                            egui::Button::new(format!("Quick state #{}", i + 1)),
                        )
                        .clicked()
                    {
                        load_file = Some(p.unwrap().clone());
                        ui.close_menu();
                    }
                }
                if let Some(p) = load_file {
                    self.load_statefile(p);
                }
                ui.separator();
                ui.strong("Quick save states");
                for (i, p) in self.quick_states.iter_mut().enumerate() {
                    if ui
                        .add_enabled(
                            self.emu.is_initialized(),
                            egui::Button::new(format!("Quick state #{}", i + 1)),
                        )
                        .clicked()
                    {
                        let mut path = env::temp_dir();
                        path.push(format!(
                            "snow_quickstate_{}_{}.snows",
                            std::process::id(),
                            i
                        ));
                        if !self.temp_files.contains(&path) {
                            self.temp_files.push(path.clone());
                        }
                        self.emu.save_state(&path, None);
                        *p = Some(path);
                        ui.close_menu();
                    }
                }
                ui.separator();
                ui.checkbox(
                    &mut self.workspace.pause_on_state_load,
                    "Pause emulator after state load",
                );
            });

            if self.emu.is_initialized() {
                ui.menu_button("Drives", |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    self.draw_menu_floppies(ui);

                    // Needs cloning for the later borrow to call create_disk_dialog.open()
                    let targets = self.emu.get_scsi_target_status().map(|d| d.to_owned());
                    if let Some(targets) = targets {
                        ui.separator();
                        for (i, target) in targets.iter().enumerate() {
                            self.draw_scsi_target_menu(ui, i, target.as_ref(), true);
                        }
                    }
                });
            }
            ui.menu_button("Ports", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                ui.menu_button(
                    format!(
                        "{} Channel A (modem)",
                        egui_material_icons::icons::ICON_CABLE
                    ),
                    |ui| {
                        ui.set_min_width(Self::SUBMENU_WIDTH);
                        if ui
                            .checkbox(&mut self.workspace.terminal_open[0], "Terminal")
                            .clicked()
                        {
                            ui.close_menu();
                        }
                        ui.separator();
                        self.draw_serial_bridge_menu(ui, SccCh::A);
                    },
                );
                ui.menu_button(
                    format!(
                        "{} Channel B (printer)",
                        egui_material_icons::icons::ICON_CABLE
                    ),
                    |ui| {
                        ui.set_min_width(Self::SUBMENU_WIDTH);
                        if ui
                            .checkbox(&mut self.workspace.terminal_open[1], "Terminal")
                            .clicked()
                        {
                            ui.close_menu();
                        }
                        ui.separator();
                        self.draw_serial_bridge_menu(ui, SccCh::B);
                    },
                );
            });
            ui.menu_button("Tools", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                ui.menu_button("File sharing", |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH + 100.0);
                    ui.label("Shared folder:");
                    let mut shared_dir_str = self
                        .workspace
                        .get_shared_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    ui.add_enabled(false, egui::TextEdit::singleline(&mut shared_dir_str));
                    if ui
                        .add_enabled(
                            self.emu.is_initialized(),
                            egui::Button::new("Select folder..."),
                        )
                        .clicked()
                    {
                        self.shared_dir_dialog.pick_directory();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            !shared_dir_str.is_empty(),
                            egui::Button::new("Disable sharing"),
                        )
                        .clicked()
                    {
                        self.workspace.set_shared_dir(None);
                        self.emu.set_shared_dir(None);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.emu.is_initialized(),
                            egui::Button::new("Insert toolbox floppy"),
                        )
                        .clicked()
                    {
                        self.emu.load_toolbox_floppy();
                        ui.close_menu();
                    }
                });
                ui.separator();
                if ui
                    .add_enabled(
                        self.emu.is_initialized(),
                        egui::Button::new("Take screenshot"),
                    )
                    .clicked()
                {
                    self.screenshot();
                    ui.close_menu();
                }
                ui.separator();
                if !self.emu.is_recording_input() {
                    if ui
                        .add_enabled(
                            self.emu.is_initialized(),
                            egui::Button::new("Record input..."),
                        )
                        .clicked()
                    {
                        self.record_dialog.save_file();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            self.emu.is_initialized(),
                            egui::Button::new("Replay recording..."),
                        )
                        .clicked()
                    {
                        self.record_dialog.pick_file();
                        ui.close_menu();
                    }
                } else if ui.button("Stop recording").clicked() {
                    self.emu.record_input_end();
                    ui.close_menu();
                }
            });
            ui.menu_button("Options", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                ui.menu_button("UI scale", |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    for z in Self::ZOOM_FACTORS {
                        if ui.button(format!("{:0.2}", z)).clicked() {
                            ctx.set_zoom_factor(z);
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                ui.strong("Viewport options");
                ui.add(
                    egui::Slider::new(&mut self.framebuffer.scale, 0.5..=4.0).text("Display scale"),
                );
                ui.menu_button("Display position", |ui| {
                    if ui
                        .radio_value(
                            &mut self.workspace.framebuffer_mode,
                            FramebufferMode::CenteredHorizontally,
                            "Centered horizontally",
                        )
                        .clicked()
                    {
                        ui.close_menu();
                    }
                    if ui
                        .radio_value(
                            &mut self.workspace.framebuffer_mode,
                            FramebufferMode::Centered,
                            "Centered",
                        )
                        .clicked()
                    {
                        ui.close_menu();
                    }
                    if ui
                        .radio_value(
                            &mut self.workspace.framebuffer_mode,
                            FramebufferMode::Detached,
                            "Detached",
                        )
                        .clicked()
                    {
                        ui.close_menu();
                    }
                });
                ui.menu_button("Scaling algorithm", |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    for algorithm in ScalingAlgorithm::iter() {
                        ui.radio_value(
                            &mut self.framebuffer.scaling_algorithm,
                            algorithm,
                            format!("{}", algorithm),
                        );
                    }
                });

                if ui
                    .add_enabled(
                        matches!(
                            self.emu.get_model(),
                            Some(MacModel::Early128K)
                                | Some(MacModel::Early512K)
                                | Some(MacModel::Early512Ke)
                                | Some(MacModel::Plus)
                                | Some(MacModel::SE)
                                | Some(MacModel::SeFdhd)
                                | Some(MacModel::Classic)
                        ),
                        egui::Checkbox::new(
                            &mut self.emu.debug_framebuffers,
                            "Show all framebuffers",
                        ),
                    )
                    .clicked()
                {
                    self.emu.set_debug_framebuffers(self.emu.debug_framebuffers);
                    ui.close_menu();
                }
                ui.add(egui::Checkbox::new(
                    &mut self.framebuffer.shader_enabled,
                    "Shader effects",
                ));
                if self.framebuffer.shader_enabled {
                    ui.menu_button("Shader effect settings", |ui| {
                        ui.set_min_width(Self::SUBMENU_WIDTH);

                        let mut move_action: Option<(usize, bool)> = None; // (index, move_up)
                        let mut remove_index: Option<usize> = None;
                        let mut add_shader: Option<crate::shader_pipeline::ShaderId> = None;

                        // Dynamically generate UI for each shader in the pipeline
                        let shader_count = self.framebuffer.shader_config_count();
                        for i in 0..shader_count {
                            let config = &mut self.framebuffer.shader_configs_mut()[i];
                            let heading = format!(
                                "{}. {} ({})",
                                i + 1,
                                config.id.display_name(),
                                if config.enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                }
                            );
                            ui.collapsing(heading, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            i > 0,
                                            egui::Button::new(
                                                egui_material_icons::icons::ICON_ARROW_UPWARD,
                                            ),
                                        )
                                        .clicked()
                                    {
                                        move_action = Some((i, true));
                                    }

                                    if ui
                                        .add_enabled(
                                            i < shader_count - 1,
                                            egui::Button::new(
                                                egui_material_icons::icons::ICON_ARROW_DOWNWARD,
                                            ),
                                        )
                                        .clicked()
                                    {
                                        move_action = Some((i, false));
                                    }

                                    if ui.button(egui_material_icons::icons::ICON_DELETE).clicked()
                                    {
                                        remove_index = Some(i);
                                    }

                                    ui.checkbox(&mut config.enabled, "Enabled");
                                });

                                ui.separator();

                                // Get cached parameter metadata
                                let params = config.id.parameters();

                                // Generate sliders for each parameter
                                for param in params {
                                    let value = config
                                        .parameters
                                        .entry(param.name.clone())
                                        .or_insert(param.default);

                                    let mut slider =
                                        egui::Slider::new(value, param.min..=param.max)
                                            .step_by(param.step as f64)
                                            .text(&param.display_name);

                                    // Special formatter for MASK parameter
                                    if param.name == "MASK" {
                                        slider = slider.custom_formatter(|n, _| match n as i32 {
                                            0 => "None".to_string(),
                                            1 => "Aperture Grille".to_string(),
                                            2 => "Aperture Grille Lite".to_string(),
                                            3 => "Shadow Mask".to_string(),
                                            _ => n.to_string(),
                                        });
                                    }

                                    ui.add(slider);
                                }
                            });
                        }

                        // Add shader menu
                        let available_shaders = self.framebuffer.available_shaders();
                        if !available_shaders.is_empty() {
                            ui.separator();
                            ui.menu_button("Add shader", |ui| {
                                ui.set_min_width(Self::SUBMENU_WIDTH);

                                for id in available_shaders {
                                    if ui.button(id.display_name()).clicked() {
                                        add_shader = Some(id);
                                        ui.close_menu();
                                    }
                                }
                            });
                        }

                        if let Some((index, move_up)) = move_action {
                            if move_up {
                                self.framebuffer.move_shader_up(index);
                            } else {
                                self.framebuffer.move_shader_down(index);
                            }
                        }
                        if let Some(index) = remove_index {
                            self.framebuffer.remove_shader(index);
                        }
                        if let Some(id) = add_shader {
                            self.framebuffer.add_shader(id);
                        }
                    });
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.workspace.map_cmd_ralt, "Map right ALT to Cmd")
                    .clicked()
                {
                    ui.close_menu();
                }

                ui.separator();
                if ui
                    .checkbox(
                        &mut self.workspace.disassembly_labels,
                        "Show labels in disassembly",
                    )
                    .clicked()
                {
                    ui.close_menu();
                }
            });
            ui.menu_button("View", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                if ui
                    .add_enabled(
                        self.emu.is_initialized(),
                        egui::Button::new("Enter fullscreen"),
                    )
                    .clicked()
                {
                    self.enter_fullscreen(ctx);
                    ui.close_menu();
                }
                if ui
                    .add_enabled(
                        self.emu.is_initialized(),
                        egui::Button::new("Enter Zen mode"),
                    )
                    .clicked()
                {
                    self.enter_zen_mode();
                    ui.close_menu();
                }
                ui.separator();
                if ui.checkbox(&mut self.workspace.log_open, "Log").clicked() {
                    ui.close_menu();
                }
                if ui
                    .checkbox(&mut self.workspace.disassembly_open, "Disassembly")
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(
                        &mut self.workspace.instruction_history_open,
                        "Instruction history",
                    )
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(
                        &mut self.workspace.systrap_history_open,
                        "System trap history",
                    )
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(&mut self.workspace.registers_open, "Registers")
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(&mut self.workspace.breakpoints_open, "Breakpoints")
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(&mut self.workspace.memory_open, "Memory")
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(&mut self.workspace.watchpoints_open, "Watchpoints")
                    .clicked()
                {
                    ui.close_menu();
                }
                if ui
                    .checkbox(&mut self.workspace.peripheral_debug_open, "Peripherals")
                    .clicked()
                {
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Reset layout").clicked() {
                    self.workspace.reset_windows();
                    self.load_windows = true;
                    ui.close_menu();
                }
            });
            ui.menu_button("Help", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);

                if ui.button("Documentation...").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab("https://docs.snowemu.com/"));
                    ui.close_menu();
                }
                if ui.button("Website...").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab("https://snowemu.com/"));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Report an issue...").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(
                        "https://github.com/twvd/snow/issues/new/choose",
                    ));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("About Snow").clicked() {
                    self.about_dialog.open();
                    ui.close_menu();
                }
            });
        });
    }

    fn draw_scsi_target_menu(
        &mut self,
        ui: &mut egui::Ui,
        id: usize,
        target: Option<&snow_core::emulator::comm::ScsiTargetStatus>,
        show_detach: bool,
    ) {
        if let Some(target) = target {
            match target.target_type {
                ScsiTargetType::Disk => {
                    ui.menu_button(
                        format!(
                            "{} SCSI #{}: HDD {} ({:0.2}MB)",
                            egui_material_icons::icons::ICON_HARD_DRIVE_2,
                            id,
                            target
                                .image
                                .as_ref()
                                .unwrap()
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            target.capacity.unwrap() / 1024 / 1024
                        ),
                        |ui| {
                            ui.set_min_width(Self::SUBMENU_WIDTH);
                            if ui.button("Detach hard drive").clicked() {
                                self.emu.scsi_detach_target(id);
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Branch off image...").clicked() {
                                self.hdd_dialog_idx = id;
                                self.hdd_dialog.save_file();
                                ui.close_menu();
                            }
                        },
                    );
                }
                ScsiTargetType::Cdrom => {
                    if let Some(image) = target.image.as_ref() {
                        // CD inserted
                        ui.menu_button(
                            format!(
                                "{} SCSI #{}: CD {}",
                                egui_material_icons::icons::ICON_ALBUM,
                                id,
                                image.file_name().unwrap_or_default().to_string_lossy(),
                            ),
                            |ui| {
                                ui.set_min_width(Self::SUBMENU_WIDTH);
                                if show_detach {
                                    if ui.button("Detach CD-ROM drive").clicked() {
                                        self.emu.scsi_detach_target(id);
                                        ui.close_menu();
                                    }
                                } else {
                                    ui.disable();
                                    let _ = ui.button("No actions");
                                }
                            },
                        );
                    } else {
                        ui.menu_button(
                            format!(
                                "{} SCSI #{}: CD (no media)",
                                egui_material_icons::icons::ICON_EJECT,
                                id,
                            ),
                            |ui| {
                                ui.set_min_width(Self::SUBMENU_WIDTH);
                                if ui.button("Load image...").clicked() {
                                    self.cdrom_dialog_idx = id;
                                    self.cdrom_dialog.pick_file();
                                    ui.close_menu();
                                }
                                ui.menu_button("Load recent image", |ui| {
                                    ui.set_min_width(Self::SUBMENU_WIDTH);
                                    for (idx, path, display_name) in
                                        self.settings.get_recent_cd_images_for_display()
                                    {
                                        if ui.button(format!("{}: {}", idx, display_name)).clicked()
                                        {
                                            self.emu.scsi_load_cdrom(id, &path);
                                            self.settings.add_recent_cd_image(&path);
                                            ui.close_menu();
                                        }
                                    }
                                    if self.settings.recent_cd_images.is_empty() {
                                        ui.weak("No recent images");
                                    }
                                });
                                if ui.button("Mount image from files...").clicked() {
                                    self.cdrom_dialog_idx = id;
                                    self.cdrom_files_dialog.pick_multiple();
                                    ui.close_menu();
                                }
                                if show_detach {
                                    ui.separator();
                                    if ui.button("Detach CD-ROM drive").clicked() {
                                        self.emu.scsi_detach_target(id);
                                        ui.close_menu();
                                    }
                                }
                            },
                        );
                    }
                }
                #[cfg(feature = "ethernet")]
                ScsiTargetType::Ethernet => {
                    ui.menu_button(
                        format!(
                            "{} SCSI #{}: Ethernet",
                            egui_material_icons::icons::ICON_SETTINGS_ETHERNET,
                            id,
                        ),
                        |ui| {
                            use snow_core::mac::scsi::ethernet::EthernetLinkType;

                            ui.set_min_width(Self::SUBMENU_WIDTH);
                            ui.strong("Link type");

                            let link_type = target.link_type.clone().unwrap();
                            let mut new_link_type = link_type.clone();
                            ui.radio_value(&mut new_link_type, EthernetLinkType::Down, "Down");
                            #[cfg(feature = "ethernet_nat")]
                            {
                                ui.radio_value(&mut new_link_type, EthernetLinkType::NAT, "NAT");
                            }
                            #[cfg(feature = "ethernet_raw")]
                            {
                                for interface in pnet::datalink::interfaces()
                                    .into_iter()
                                    .filter(|i| i.is_up() && !i.ips.is_empty() && !i.is_loopback())
                                {
                                    ui.radio_value(
                                        &mut new_link_type,
                                        EthernetLinkType::Bridge(interface.index),
                                        format!("Bridge: {}", interface.name),
                                    );
                                }
                            }
                            #[cfg(all(feature = "ethernet_tap", target_os = "linux"))]
                            {
                                let tap_devices: Vec<_> = pnet::datalink::interfaces()
                                    .into_iter()
                                    .filter(|i| {
                                        i.name.starts_with("tap") || i.name.starts_with("snow")
                                    })
                                    .collect();

                                if tap_devices.is_empty() {
                                    ui.weak("TAP devices: No TAP devices found");
                                } else {
                                    for interface in tap_devices {
                                        ui.radio_value(
                                            &mut new_link_type,
                                            EthernetLinkType::TapBridge(interface.name.clone()),
                                            format!("TAP device: {}", interface.name),
                                        );
                                    }
                                }
                            }

                            if new_link_type != link_type {
                                self.emu.set_eth_link(id, new_link_type);
                            }

                            ui.separator();
                            if ui.button("Detach").clicked() {
                                self.emu.scsi_detach_target(id);
                                ui.close_menu();
                            }
                        },
                    );
                }
            }
        } else {
            ui.menu_button(
                format!(
                    "{} SCSI #{}: (no device)",
                    egui_material_icons::icons::ICON_BLOCK,
                    id
                ),
                |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH + 50.0);
                    if ui.button("Create new HDD image...").clicked() {
                        self.create_disk_dialog.open(id, &self.workspace_dir());
                        ui.close_menu();
                    }
                    if ui.button("Load HDD disk image...").clicked() {
                        self.hdd_dialog_idx = id;
                        self.hdd_dialog.pick_file();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Attach CD-ROM drive (with image)...").clicked() {
                        self.cdrom_dialog_idx = id;
                        self.cdrom_dialog.pick_file();
                        ui.close_menu();
                    }
                    if ui.button("Attach CD-ROM drive (empty)").clicked() {
                        self.emu.scsi_attach_cdrom(id);
                        ui.close_menu();
                    }
                    #[cfg(feature = "ethernet")]
                    {
                        ui.separator();
                        if ui
                            .add_enabled(
                                !self
                                    .emu
                                    .get_scsi_targets()
                                    .map(|t| {
                                        t.iter().any(|i| {
                                            matches!(i.target_type, Some(ScsiTargetType::Ethernet))
                                        })
                                    })
                                    .unwrap_or(false),
                                egui::Button::new("Attach Ethernet controller"),
                            )
                            .clicked()
                        {
                            self.emu.scsi_attach_ethernet(id);
                            ui.close_menu();
                        }
                    }
                },
            );
        }
    }

    fn draw_serial_bridge_menu(&self, ui: &mut egui::Ui, ch: SccCh) {
        let is_enabled = self.emu.is_serial_bridge_enabled(ch);
        let emu_ready = self.emu.is_initialized();

        if is_enabled {
            // Bridge is active - show status and disable option
            if let Some(status) = self.emu.get_serial_bridge_status(ch) {
                ui.label(format!("Bridge: {}", status));
            }
            if ui
                .add_enabled(emu_ready, egui::Button::new("Disable bridge"))
                .clicked()
            {
                let _ = self.emu.disable_serial_bridge(ch);
                ui.close_menu();
            }
        } else {
            // Bridge is inactive - show enable options
            ui.strong("Serial Bridge");

            #[cfg(unix)]
            if ui
                .add_enabled(emu_ready, egui::Button::new("Enable PTY bridge"))
                .clicked()
            {
                let _ = self.emu.enable_serial_bridge(ch, SerialBridgeConfig::Pty);
                ui.close_menu();
            }

            let port = match ch {
                SccCh::A => 1984,
                SccCh::B => 1985,
            };
            if ui
                .add_enabled(
                    emu_ready,
                    egui::Button::new(format!("Enable TCP bridge (port {})", port)),
                )
                .clicked()
            {
                let _ = self
                    .emu
                    .enable_serial_bridge(ch, SerialBridgeConfig::Tcp(port));
                ui.close_menu();
            }

            if ui
                .add_enabled(emu_ready, egui::Button::new("Enable LocalTalk (UDP)"))
                .clicked()
            {
                let _ = self
                    .emu
                    .enable_serial_bridge(ch, SerialBridgeConfig::LocalTalk);
                ui.close_menu();
            }
        }
    }

    fn draw_menu_floppies(&mut self, ui: &mut egui::Ui) {
        for (i, d) in (0..3).filter_map(|i| self.emu.get_fdd_status(i).map(|d| (i, d))) {
            ui.menu_button(
                format!(
                    "{} Floppy #{}: {}",
                    if d.ejected {
                        egui_material_icons::icons::ICON_EJECT
                    } else if !d.ejected && d.dirty {
                        egui_material_icons::icons::ICON_SAVE_AS
                    } else {
                        egui_material_icons::icons::ICON_SAVE
                    },
                    i + 1,
                    if d.ejected {
                        "(ejected)"
                    } else {
                        &d.image_title
                    }
                ),
                |ui| {
                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    if ui.button("Insert blank 400/800K floppy").clicked() {
                        self.emu.insert_blank_floppy(i, FloppyType::Mac800K);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            self.emu.get_model().unwrap().fdd_hd(),
                            egui::Button::new("Insert blank 1.44MB floppy"),
                        )
                        .clicked()
                    {
                        self.emu.insert_blank_floppy(i, FloppyType::Mfm144M);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Load image...").clicked() {
                        self.floppy_dialog_target = FloppyDialogTarget::Drive(i);
                        self.floppy_dialog.pick_file();
                        ui.close_menu();
                    }
                    ui.menu_button("Load recent image", |ui| {
                        ui.set_min_width(Self::SUBMENU_WIDTH);
                        for (idx, path, display_name) in
                            self.settings.get_recent_floppy_images_for_display()
                        {
                            if ui.button(format!("{}: {}", idx, display_name)).clicked() {
                                self.emu.load_floppy(i, &path, false);
                                self.settings.add_recent_floppy_image(&path);
                                ui.close_menu();
                            }
                        }
                        if self.settings.recent_floppy_images.is_empty() {
                            ui.weak("No recent images");
                        }
                    });
                    if ui
                        .add_enabled(
                            self.emu.last_images[i].borrow().is_some(),
                            egui::Button::new("Re-insert last ejected floppy"),
                        )
                        .clicked()
                    {
                        self.emu.reload_floppy(i);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(!d.ejected && d.dirty, egui::Button::new("Save image..."))
                        .clicked()
                    {
                        self.floppy_dialog_target = FloppyDialogTarget::Drive(i);
                        self.floppy_dialog.save_file();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            self.emu.last_images[i].borrow().is_some(),
                            egui::Button::new("Save last ejected image..."),
                        )
                        .clicked()
                    {
                        let img = self.emu.last_images[i].borrow().clone().unwrap();
                        self.floppy_dialog_target = FloppyDialogTarget::Image(img);
                        self.floppy_dialog.save_file();
                        ui.close_menu();
                    }
                    ui.separator();

                    if ui
                        .add_enabled(!d.ejected, egui::Button::new("Force eject"))
                        .clicked()
                    {
                        self.emu.force_eject(i);
                        ui.close_menu();
                    }

                    if d.drive_type.has_pwm_control() {
                        ui.separator();
                        ui.label("Simulate drive RPM variance");
                        if ui
                            .add(
                                egui::Slider::new(&mut self.floppy_rpm_adjustment[i], -100..=100)
                                    .suffix(" RPM"),
                            )
                            .changed()
                        {
                            self.emu
                                .set_floppy_rpm_adjustment(i, self.floppy_rpm_adjustment[i]);
                        }
                    }
                },
            );
        }
    }

    fn draw_toolbar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::new(24.0, eframe::epaint::FontFamily::Proportional),
            );

            if ui
                .add(egui::Button::new(egui_material_icons::icons::ICON_MEMORY))
                .on_hover_text("Load ROM...")
                .clicked()
            {
                self.model_dialog.open(
                    self.settings.get_last_roms(),
                    self.settings.get_last_display_roms(),
                );
            }
            if self.emu.is_initialized() {
                ui.separator();
                if ui
                    .add(egui::Button::new(
                        egui_material_icons::icons::ICON_RESTART_ALT,
                    ))
                    .on_hover_text("Reset machine")
                    .clicked()
                {
                    self.emu.reset();
                    ui.close_menu();
                }

                if self.emu.is_running() {
                    if ui
                        .add(egui::Button::new(egui_material_icons::icons::ICON_PAUSE))
                        .on_hover_text("Pause execution")
                        .clicked()
                    {
                        self.emu.stop();
                    }
                } else if ui
                    .add(egui::Button::new(
                        egui_material_icons::icons::ICON_PLAY_ARROW,
                    ))
                    .on_hover_text("Resume execution")
                    .clicked()
                {
                    self.emu.run();
                }

                if ui
                    .add_enabled(
                        self.emu.is_running(),
                        egui::Button::new(egui_material_icons::icons::ICON_FAST_FORWARD)
                            .selected(self.emu.is_fastforward()),
                    )
                    .on_hover_text("Fast-forward execution")
                    .clicked()
                {
                    self.emu.toggle_fastforward();
                }

                if ui
                    .add_enabled(
                        !self.emu.is_running(),
                        egui::Button::new(egui_material_icons::icons::ICON_STEP_INTO),
                    )
                    .on_hover_text("Step into")
                    .clicked()
                {
                    self.emu.step();
                }
                if ui
                    .add_enabled(
                        !self.emu.is_running(),
                        egui::Button::new(egui_material_icons::icons::ICON_STEP_OVER),
                    )
                    .on_hover_text("Step over")
                    .clicked()
                {
                    self.emu.step_over();
                }
                if ui
                    .add_enabled(
                        !self.emu.is_running(),
                        egui::Button::new(egui_material_icons::icons::ICON_STEP_OUT),
                    )
                    .on_hover_text("Step out")
                    .clicked()
                {
                    self.emu.step_out();
                }

                ui.separator();
                let audio_muted = self.emu.audio_is_muted();
                let audio_slow = self.emu.audio_is_slow();
                if ui
                    .add_enabled(
                        !audio_slow,
                        egui::Button::new(if audio_muted {
                            egui_material_icons::icons::ICON_VOLUME_OFF
                        } else {
                            egui_material_icons::icons::ICON_VOLUME_UP
                        }),
                    )
                    .on_hover_text(if audio_muted {
                        "Unmute audio"
                    } else {
                        "Mute audio"
                    })
                    .on_disabled_hover_text(
                        "Audio has been disabled because the emulator is \
                                            paused or performance is insufficient",
                    )
                    .clicked()
                {
                    self.emu.audio_mute(!audio_muted);
                }
                if ui
                    .add(egui::Button::new(
                        egui_material_icons::icons::ICON_PHOTO_CAMERA,
                    ))
                    .on_hover_text("Take screenshot")
                    .clicked()
                {
                    self.screenshot();
                }
                if ui
                    .add(egui::Button::new(
                        egui_material_icons::icons::ICON_FULLSCREEN,
                    ))
                    .on_hover_text("Enter fullscreen")
                    .clicked()
                {
                    self.enter_fullscreen(ctx);
                }
                if ui
                    .add(egui::Button::new(
                        egui_material_icons::icons::ICON_FILTER_CENTER_FOCUS,
                    ))
                    .on_hover_text("Enter Zen mode")
                    .clicked()
                {
                    self.enter_zen_mode();
                }
            }
        });
    }

    fn default_dir() -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| env::current_dir().unwrap())
    }

    fn workspace_dir(&self) -> PathBuf {
        self.workspace_file
            .clone()
            .map(|f| f.parent().unwrap().to_path_buf())
            .unwrap_or_else(Self::default_dir)
    }

    pub fn show_error(&mut self, text: &impl std::fmt::Display) {
        self.error_dialog_open = true;
        self.error_string = text.to_string();
    }

    fn poll_winit_events(&self, ctx: &egui::Context) {
        if self.wev_recv.is_empty() {
            return;
        }

        while let Ok(wevent) = self.wev_recv.try_recv() {
            use egui_winit::winit::event::{KeyEvent, WindowEvent};
            use egui_winit::winit::keyboard::PhysicalKey;

            if !self.ui_active {
                continue;
            }

            match wevent {
                WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            physical_key: PhysicalKey::Code(kc),
                            state,
                            repeat: false,
                            ..
                        },
                    ..
                } => {
                    if ctx.wants_keyboard_input() {
                        continue;
                    }

                    if let Some(k) = map_winit_keycode(kc, self.workspace.map_cmd_ralt) {
                        self.emu.update_key(k, state.is_pressed());
                    } else {
                        log::warn!("Unknown key {:?}", kc);
                    }
                }
                _ => (),
            }
        }
    }

    fn get_machine_mouse_pos(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        if !self.framebuffer.has_pointer() {
            return None;
        }

        let mouse_pos = ctx.pointer_latest_pos()?;
        let display_size = egui::Vec2::from(self.framebuffer.display_size());
        let fbrect = self.framebuffer.rect();
        let scale = self.framebuffer.scaling_factors_actual();
        let x = (mouse_pos.x - fbrect.left_top().x) * scale.x;
        let y = (mouse_pos.y - fbrect.left_top().y) * scale.y;
        if x < 0.0 || y < 0.0 || x > display_size.x || y > display_size.y {
            None
        } else {
            Some(egui::Pos2::from([x, y]))
        }
    }

    fn update_titlebar(&self, ctx: &egui::Context) {
        let wsname = self
            .workspace_file
            .as_ref()
            .and_then(|v| v.file_stem())
            .map(|v| v.to_string_lossy())
            .unwrap_or(std::borrow::Cow::Borrowed("Untitled workspace"));

        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            if let Some(m) = self.emu.get_model() {
                format!(
                    "Snow v{} - {} - {} ({})",
                    snow_core::build_version(),
                    wsname,
                    m,
                    if self.emu.is_running() {
                        "running"
                    } else {
                        "stopped"
                    }
                )
            } else {
                format!("Snow v{} - {}", snow_core::build_version(), wsname)
            },
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn load_rom_from_path(
        &mut self,
        path: &Path,
        display_rom_path: Option<&Path>,
        extension_rom_path: Option<&Path>,
        scsi_targets: Option<ScsiTargets>,
        pram_path: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
    ) {
        // Parse custom datetime from workspace if specified
        let custom_datetime = self.workspace.custom_datetime.as_ref().and_then(|s| {
            // Try parsing with time first, then date-only
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(12, 0, 0).unwrap())
                })
                .inspect_err(|e| {
                    log::warn!("Failed to parse custom_datetime '{}': {}", s, e);
                })
                .ok()
        });

        match self.emu.init_from_rom(
            path,
            display_rom_path,
            extension_rom_path,
            scsi_targets,
            pram_path,
            args,
            model,
            self.workspace.get_shared_dir(),
            custom_datetime,
        ) {
            Ok(p) => {
                self.framebuffer
                    .load_shader_defaults(self.emu.get_model().unwrap());
                self.framebuffer.connect_receiver(p.frame_receiver);
            }
            Err(e) => self.show_error(&format!("Failed to load ROM file: {}", e)),
        }

        // Save to last used ROMs
        if let Some(model) = model {
            self.settings.set_last_rom(model, path);
            if let Some(dr_path) = display_rom_path {
                self.settings.set_last_display_rom(model, dr_path);
            }
        }

        self.workspace.set_rom_path(path);
        self.workspace.set_display_card_rom_path(display_rom_path);
        self.workspace.set_extension_rom_path(extension_rom_path);
        self.workspace.set_pram_path(pram_path);
        self.workspace.init_args = args.clone();
        self.workspace.model = model;
    }

    fn load_workspace(&mut self, path: Option<&Path>) {
        if let Some(path) = path {
            match Workspace::from_file(path) {
                Ok(ws) => {
                    self.workspace = ws;
                    self.workspace_file = Some(path.to_path_buf());
                    self.settings.add_recent_workspace(path);
                }
                Err(e) => self.show_error(&format!("Failed to load workspace: {}", e)),
            }
        } else {
            // Clean workspace
            self.workspace = Default::default();
            self.workspace_file = None;
            self.workspace_dialog.config_mut().default_file_name = String::new();
            self.workspace_dialog.config_mut().initial_directory = Self::default_dir();
        }

        // Re-initialize stuff from newly loaded workspace
        self.load_windows = true;
        self.framebuffer.scale = self.workspace.viewport_scale;
        self.framebuffer.scaling_algorithm = self.workspace.scaling_algorithm;
        self.framebuffer.shader_enabled = self.workspace.shader_enabled;
        if let Some(rompath) = self.workspace.get_rom_path() {
            let display_rom_path = self.workspace.get_display_card_rom_path();
            let extension_rom_path = self.workspace.get_extension_rom_path();
            let scsi_targets = self.workspace.scsi_targets();
            let pram_path = self.workspace.get_pram_path();
            let init_args = self.workspace.init_args.clone();
            let model = self.workspace.model;

            self.load_rom_from_path(
                &rompath,
                display_rom_path.as_deref(),
                extension_rom_path.as_deref(),
                Some(scsi_targets),
                pram_path.as_deref(),
                &init_args,
                model,
            );

            if let Some(floppy_path) = self.workspace.get_floppy_images().first() {
                if floppy_path.exists() && !self.emu.load_floppy_firstfree(floppy_path) {
                    self.show_error(&format!(
                        "Cannot load floppy image: no free drive for {:?}",
                        floppy_path
                    ));
                }
            }
        } else {
            self.emu.deinit();
        }

        // Do this after the emulator is initialized so the shader defaults that get loaded are
        // overwritten.
        if !self.workspace.shader_configs.is_empty() {
            self.framebuffer
                .import_config(self.workspace.shader_configs.clone());
        }
    }

    fn save_workspace(&mut self, path: &Path) {
        self.workspace.viewport_scale = self.framebuffer.scale;
        self.workspace.scaling_algorithm = self.framebuffer.scaling_algorithm;
        self.workspace.shader_enabled = self.framebuffer.shader_enabled;
        self.workspace.shader_configs = self.framebuffer.export_config();
        if let Some(targets) = self.emu.get_scsi_target_status().as_ref() {
            for (i, d) in targets.iter().enumerate() {
                self.workspace.set_scsi_target(i, d.clone());
            }
        }
        if let Err(e) = self.workspace.write_file(path) {
            self.show_error(&format!("Failed to save workspace: {}", e));
        }
    }

    fn sync_windows(&mut self, ctx: &egui::Context) {
        if self.load_windows {
            ctx.memory_mut(|m| m.reset_areas());
            self.load_windows = false;
            return;
        }
        ctx.memory(|m| {
            for &n in Workspace::WINDOW_NAMES {
                if let Some(r) = m.area_rect(egui::Id::from(n)) {
                    self.workspace.save_window(n, r);
                }
            }
        });
    }

    fn floppy_dialog_side_update(&mut self, d: Option<DirectoryEntry>) -> Result<()> {
        self.floppy_dialog_last = d.clone();
        self.floppy_dialog_last_image = None;

        let Some(d) = d else { return Ok(()) };
        if !d.is_file() {
            return Ok(());
        }
        if fs::metadata(d.as_path())?.len() > (40 * 1024 * 1024) {
            return Ok(());
        }
        let data = fs::read(d.as_path())?;
        self.floppy_dialog_last_type = snow_floppy::loaders::Autodetect::detect(&data).ok();
        self.floppy_dialog_last_image =
            snow_floppy::loaders::Autodetect::load(&data, Some(d.file_name())).ok();
        if let Some(img) = self.floppy_dialog_last_image.as_ref() {
            self.floppy_dialog_wp = img.get_write_protect();
        }

        Ok(())
    }

    fn state_dialog_side_update(
        &mut self,
        ctx: &egui::Context,
        d: Option<DirectoryEntry>,
    ) -> Result<()> {
        self.state_dialog_last = d.clone();
        self.state_dialog_last_header = None;

        let Some(d) = d else { return Ok(()) };
        if !d.is_file() {
            return Ok(());
        }
        let header = load_state_header(&mut File::open(d.as_path())?)?;
        self.state_dialog_screenshot = crate::util::image::load_png_from_bytes_as_texture(
            ctx,
            &header.screenshot,
            "state_screenshot",
        )
        .map_err(|e| anyhow!("State image load failed: {:?}", e))?;
        self.state_dialog_last_header = Some(header);

        Ok(())
    }

    fn screenshot(&mut self) {
        let Some(mut p) = dirs::desktop_dir().or_else(|| std::env::current_dir().ok()) else {
            self.show_error(&"Failed finding screenshot directory");
            return;
        };

        let filename = format!(
            "Snow screenshot {}.png",
            chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
        );
        p.push(&filename);
        if let Err(e) = self.framebuffer.write_screenshot_file(p) {
            self.show_error(&format!("Failed to write screenshot: {}", e));
        }
        self.toasts.add(
            egui_toast::Toast::default()
                .text(format!("Saved screenshot to desktop as '{}'", filename))
                .options(
                    egui_toast::ToastOptions::default()
                        .duration(Self::TOAST_DURATION)
                        .show_progress(true),
                ),
        );
    }

    #[allow(clippy::needless_pass_by_value)]
    fn uniform_action(&mut self, action: UniformAction) {
        match action {
            UniformAction::None => (),
            UniformAction::AddressWatch(a, t) => {
                self.workspace.watchpoints_open = true;
                self.watchpoints.add_watchpoint(a, t, format!("{:08X}", a));
            }
            UniformAction::Breakpoint(breakpoint) => {
                self.workspace.breakpoints_open = true;
                self.emu.set_breakpoint(breakpoint);
            }
            UniformAction::AddressMemoryViewer(addr) => {
                self.workspace.memory_open = true;
                self.memory.go_to_address(addr);
            }
            UniformAction::ShowError(s) => {
                self.show_error(&s);
            }
        }
    }

    fn handle_model_selection_result(&mut self, result: &ModelSelectionResult) {
        self.load_rom_from_path(
            &result.main_rom_path,
            result.display_rom_path.as_deref(),
            result.extension_rom_path.as_deref(),
            self.emu.get_scsi_targets(),
            result.pram_path.as_deref(),
            &result.init_args,
            Some(result.model),
        );
        self.last_running = false;
    }

    fn update_snowflakes(&mut self, screen_size: egui::Vec2) {
        // Only update snowflakes if About dialog is open
        if !self.about_dialog.is_open() {
            self.snowflakes.clear();
            return;
        }

        let now = Instant::now();
        let delta_time = now.duration_since(self.last_snowflake_time).as_secs_f32();
        self.last_snowflake_time = now;

        // Spawn new snowflakes
        self.snowflake_spawn_timer += delta_time;
        if self.snowflake_spawn_timer > 0.1 {
            // Spawn every 100ms
            self.snowflake_spawn_timer = 0.0;
            if self.snowflakes.len() < 50 {
                // Limit to 50 snowflakes
                self.snowflakes.push(Snowflake::new(screen_size.x));
            }
        }

        // Update existing snowflakes
        for snowflake in &mut self.snowflakes {
            snowflake.update(delta_time);
        }

        // Remove snowflakes that are off screen
        self.snowflakes
            .retain(|snowflake| !snowflake.is_off_screen(screen_size.y));
    }

    fn draw_snowflakes(&self, ui: &egui::Ui) {
        if self.about_dialog.is_open() {
            for snowflake in &self.snowflakes {
                snowflake.draw(ui);
            }
        }
    }

    /// This function nukes the Cmd+Q shortcut from the menubar so that this common classic
    /// MacOS shortcut doesn't lead to users inadvertently terminating the emulator.
    ///
    /// https://github.com/twvd/snow/issues/106
    #[cfg(target_os = "macos")]
    fn patch_macos_menubar(&self) {
        use objc2_app_kit::NSApplication;
        use objc2_foundation::ns_string;

        extern "C" {
            static NSApp: Option<&'static NSApplication>;
        }

        unsafe {
            if let Some(app) = NSApp {
                if let Some(main_menu) = app.mainMenu() {
                    // Find the first menubar dropdown ('Snow')
                    if let Some(item) = main_menu.itemAtIndex(0) {
                        if let Some(app_menu) = item.submenu() {
                            // Find the quit item, last one in the menu
                            if let Some(quit) = app_menu.itemAtIndex(app_menu.numberOfItems() - 1) {
                                // Remove the keyboard shortcut so Cmd+Q doesn't terminate the emulator
                                quit.setKeyEquivalent(ns_string!(""));
                            }
                        }
                    }
                }
            }
        }
    }

    fn create_temp_iso<P: AsRef<Path>>(paths: &[P]) -> Result<PathBuf> {
        // ISO-9660 interchange level 2 is max 31 characters
        const MAX_FILENAME_LEN: usize = 30;

        let mut files = hadris_iso::FileInput::empty();
        for p in paths.iter().map(|p| p.as_ref()) {
            if !p.is_file() {
                log::warn!("Skipping {}: not a file", p.display());
                continue;
            }

            let stem = p
                .file_stem()
                .map(|p| p.to_string_lossy().to_string())
                .context("Failed to parse filename")?;

            // Sanitize the extension to uppercase and max 3 characters alpha-numeric
            let mut extension = p
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_uppercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(3)
                .collect::<String>();
            if !extension.is_empty() {
                extension = format!(".{}", extension);
            }

            // Sanitize the filename as per ISO-9660 allowed characters and extension
            let mut new_filename = stem
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            assert!(extension.len() <= 4);

            // Truncate to maximum length, before the extension
            let max_len = MAX_FILENAME_LEN - extension.len();
            if new_filename.len() > max_len {
                // We have to truncate
                for n in 1.. {
                    let suffix = format!("_{}", n);
                    let truncated_filename =
                        format!("{}{}", &new_filename[..(max_len - suffix.len())], suffix);
                    let check_filename = format!("{}{}", truncated_filename, extension);
                    if files.get(&check_filename).is_none() {
                        new_filename = truncated_filename;
                        break;
                    }
                }
            }
            new_filename.push_str(&extension);
            assert!(new_filename.len() <= MAX_FILENAME_LEN);

            files.append(hadris_iso::File {
                path: new_filename,
                data: hadris_iso::FileData::File(p.to_path_buf()),
            });
        }
        let options = hadris_iso::FormatOption::default()
            .with_files(files)
            .with_volume_name("SNOW".to_string());
        let mut imgpath = env::temp_dir();
        imgpath.push(format!("snow_tmp_iso_{}.iso", rand::rng().random::<u64>()));
        hadris_iso::IsoImage::format_file(&imgpath, options)?;
        Ok(imgpath)
    }

    fn load_statefile<P: AsRef<Path>>(&mut self, path: P) {
        match self
            .emu
            .init_from_statefile(path.as_ref(), self.workspace.pause_on_state_load)
        {
            Ok(p) => {
                self.framebuffer
                    .load_shader_defaults(self.emu.get_model().unwrap());
                self.framebuffer.connect_receiver(p.frame_receiver);
            }
            Err(e) => self.show_error(&format!("Failed to load state file: {:?}", e)),
        }
    }

    fn load_dropped_file(&mut self, path: &Path) {
        let Some(ext) = path
            .extension()
            .map(|p| p.to_ascii_lowercase().to_string_lossy().to_string())
        else {
            self.toasts.add(
                Toast::new()
                    .text("Unrecognized file dropped")
                    .options(
                        egui_toast::ToastOptions::default()
                            .duration(Self::TOAST_DURATION)
                            .show_progress(true),
                    )
                    .kind(ToastKind::Warning),
            );
            return;
        };

        // Try to detect and load floppy images
        if snow_floppy::loaders::ImageType::EXTENSIONS
            .into_iter()
            .any(|s| ext.eq_ignore_ascii_case(s))
        {
            let Ok(md) = fs::metadata(path) else {
                return;
            };
            if md.len() < (40 * 1024 * 1024) {
                let Ok(image) = fs::read(path) else {
                    return;
                };
                if snow_floppy::loaders::Autodetect::detect(&image).is_ok() {
                    if !self.emu.load_floppy_firstfree(path) {
                        self.toasts.add(
                            Toast::new()
                                .text("Cannot load floppy image: no free drive")
                                .kind(ToastKind::Error),
                        );
                    }
                    return;
                }
            }
        }

        match ext.as_ref() {
            "rom" => {
                self.load_rom_from_path(
                    path,
                    None,
                    None,
                    None,
                    None,
                    &EmulatorInitArgs::default(),
                    None,
                );
            }
            "snoww" => self.load_workspace(Some(path)),
            "snows" => self.load_statefile(path),
            "iso" | "toast" => {
                if !self.emu.scsi_load_cdrom_firstfree(path) {
                    self.toasts.add(
                        Toast::new()
                            .text("Cannot load CD-ROM image: no free drive")
                            .kind(ToastKind::Error),
                    );
                }
            }
            "img" | "hda" => {
                // If this was a floppy image (.img), it would have been caught earlier
                // by the floppy loader already.
                if !self.emu.scsi_attach_hdd_firstfree(path) {
                    self.toasts.add(
                        Toast::new()
                            .text("Cannot load hard drive image: no free SCSI slot")
                            .kind(ToastKind::Error),
                    );
                }
            }
            _ => {
                self.toasts.add(
                    Toast::new()
                        .text("Unrecognized file dropped")
                        .options(
                            egui_toast::ToastOptions::default()
                                .duration(Self::TOAST_DURATION)
                                .show_progress(true),
                        )
                        .kind(ToastKind::Warning),
                );
            }
        }
    }
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_draw {
            #[cfg(target_os = "macos")]
            {
                self.patch_macos_menubar();
            }
            self.update_titlebar(ctx);
            self.first_draw = false;
        }

        // Check for dropped files
        // Only do this if there's no file dialogs open to avoid the drag-drop event
        // to be handled twice.
        if self.ui_active {
            ctx.input(|i| {
                for p in i.raw.dropped_files.iter().filter_map(|p| p.path.as_ref()) {
                    self.load_dropped_file(p);
                }
            });
        }

        self.sync_windows(ctx);
        self.poll_winit_events(ctx);
        self.uniform_action(UNIFORM_ACTION.take());

        if self.emu.poll() {
            // Change in emulator state
            if self.last_running != self.emu.is_running() {
                self.last_running = self.emu.is_running();
                self.update_titlebar(ctx);
            }
            self.registers.update_regs(self.emu.get_regs().clone());

            // Apply pending serial bridges from CLI args (once, when emulator initializes)
            if self.emu.is_initialized() && !self.serial_bridges_applied {
                for (idx, ch) in [SccCh::A, SccCh::B].iter().enumerate() {
                    if let Some(config) = self.pending_serial_bridges[idx].take() {
                        let _ = self.emu.enable_serial_bridge(*ch, config);
                    }
                }
                self.serial_bridges_applied = true;
            }

            while let Some((t, msg)) = self.emu.take_message() {
                self.toasts.add(match t {
                    UserMessageType::Success => Toast::default()
                        .options(
                            ToastOptions::default()
                                .show_progress(true)
                                .duration(Self::TOAST_DURATION),
                        )
                        .text(msg)
                        .kind(ToastKind::Success),
                    UserMessageType::Notice => Toast::default()
                        .options(
                            ToastOptions::default()
                                .show_progress(true)
                                .duration(Self::TOAST_DURATION),
                        )
                        .text(msg)
                        .kind(ToastKind::Info),
                    UserMessageType::Warning => Toast::default()
                        .options(
                            ToastOptions::default()
                                .show_progress(true)
                                .duration_in_seconds(5.0),
                        )
                        .text(msg)
                        .kind(ToastKind::Warning),
                    UserMessageType::Error => Toast::default()
                        .options(ToastOptions::default())
                        .text(msg)
                        .kind(ToastKind::Error),
                });
            }
            while let Some((addr, data, size)) = self.emu.take_mem_update() {
                self.memory.update_memory(addr, &data, size);
            }
        }

        self.toasts.show(ctx);

        self.ui_active = true;

        // Error modal
        let mut error_open = self.error_dialog_open;
        egui::Window::new("Error")
            .open(&mut error_open)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui_material_icons::icons::ICON_WARNING);
                    ui.label(&self.error_string);
                });
                ui.vertical_centered(|ui| {
                    if ui.button("OK").clicked() {
                        self.error_dialog_open = false;
                    }
                });
            });
        self.error_dialog_open &= error_open;
        self.ui_active &= !self.error_dialog_open;

        // Create disk image dialog
        self.create_disk_dialog.update(ctx);
        self.ui_active &= !self.create_disk_dialog.is_open();
        if let Some(result) = self.create_disk_dialog.take_result() {
            if let Err(e) = self.try_create_image(&result) {
                self.show_error(&format!("Failed to create image: {}", e));
            }
        }

        // Model selection/'Load ROM' dialog
        self.model_dialog.update(ctx);
        self.ui_active &= !self.model_dialog.is_open();
        if let Some(result) = self.model_dialog.take_result() {
            self.handle_model_selection_result(&result);
        }

        // About dialog
        self.about_dialog.update(ctx);
        self.ui_active &= !self.about_dialog.is_open();

        // Update snowflakes
        self.update_snowflakes(ctx.screen_rect().size());

        // Log window
        persistent_window!(&self, "Log")
            .open(&mut self.workspace.log_open)
            .show(ctx, |ui| {
                egui_logger::logger_ui().show(ui);
                ui.allocate_space(ui.available_size());
            });

        // Floppy image picker dialog
        let mut last = None;
        self.floppy_dialog
            .update_with_right_panel_ui(ctx, &mut |ui, dia| {
                if dia.selected_entry().is_some() {
                    last = dia.selected_entry().cloned();
                    if let Some(img) = &self.floppy_dialog_last_image {
                        let metadata = img.get_metadata();
                        egui::Grid::new("floppy_dialog_metadata").show(ui, |ui| {
                            ui.label(egui::RichText::new("Title").strong());
                            ui.label(metadata.get("title").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.label(egui::RichText::new("Subtitle").strong());
                            ui.label(metadata.get("subtitle").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.label(egui::RichText::new("Developer").strong());
                            ui.label(metadata.get("developer").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.label(egui::RichText::new("Publisher").strong());
                            ui.label(metadata.get("publisher").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.label(egui::RichText::new("Version").strong());
                            ui.label(metadata.get("version").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.label("");
                            ui.end_row();
                            ui.label(egui::RichText::new("Disk name").strong());
                            ui.label(metadata.get("disk_name").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.label(egui::RichText::new("Disk #").strong());
                            ui.label(metadata.get("disk_number").map_or("", |v| truncate(v, 20)));
                            ui.end_row();
                            ui.separator();
                            ui.separator();
                            ui.end_row();
                            ui.label(egui::RichText::new("Image type").strong());
                            ui.label(
                                self.floppy_dialog_last_type
                                    .map_or("", |i| i.as_friendly_str()),
                            );
                            ui.end_row();
                            ui.label(egui::RichText::new("Floppy type").strong());
                            ui.label(img.get_type().to_string());
                            ui.end_row();
                            ui.label(egui::RichText::new("Tracks (RF/F/B/S)").strong());
                            ui.label(format!(
                                "{}/{}/{}/{}",
                                img.count_original_track_type(OriginalTrackType::RawFlux),
                                img.count_original_track_type(OriginalTrackType::Flux),
                                img.count_original_track_type(OriginalTrackType::Bitstream),
                                img.count_original_track_type(OriginalTrackType::Sector),
                            ));
                            ui.end_row();
                        });
                        if img.count_original_track_type(OriginalTrackType::RawFlux) > 0 {
                            egui::Frame::none()
                                .fill(egui::Color32::ORANGE)
                                .inner_margin(egui::Margin::same(10.0))
                                .outer_margin(egui::Margin::same(5.0))
                                .stroke(egui::Stroke::new(2.0, egui::Color32::RED))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            "This is a raw flux image format, which is not designed for emulator use.\n\nSnow will load it, but you may encounter issues. It is recommended to convert it to a resolved flux format first (for example: MOOF).",
                                        )
                                            .strong()
                                            .color(egui::Color32::BLACK),
                                    );
                                });
                        }

                        ui.separator();
                        ui.add_enabled(
                            !img.get_write_protect(),
                            egui::Checkbox::new(
                                &mut self.floppy_dialog_wp,
                                "Mount write-protected",
                            ),
                        );
                    }
                }
            });
        if last.clone().map(|d| d.to_path_buf())
            != self.floppy_dialog_last.clone().map(|d| d.to_path_buf())
        {
            let _ = self.floppy_dialog_side_update(last);
        }
        if let Some(path) = self.floppy_dialog.take_picked() {
            match self.floppy_dialog.mode() {
                DialogMode::PickFile => {
                    let FloppyDialogTarget::Drive(driveidx) = self.floppy_dialog_target else {
                        unreachable!()
                    };
                    self.emu.load_floppy(driveidx, &path, self.floppy_dialog_wp);
                    self.settings.add_recent_floppy_image(&path);
                }
                DialogMode::SaveFile => {
                    if !path
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("moof")
                    {
                        self.show_error(&"Saved floppy image must have .MOOF extension");
                    } else {
                        match &self.floppy_dialog_target {
                            FloppyDialogTarget::Drive(driveidx) => {
                                self.emu.save_floppy(*driveidx, &path);
                            }
                            FloppyDialogTarget::Image(img) => {
                                if let Err(e) = snow_floppy::loaders::Moof::save_file(
                                    img,
                                    &path.to_string_lossy(),
                                ) {
                                    self.show_error(&format!("Failed to save image: {}", e));
                                }
                            }
                        }
                    }
                }
                _ => unreachable!(),
            }

            self.settings.fd_floppy = self.floppy_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.floppy_dialog.state() != egui_file_dialog::DialogState::Open;

        // HDD image picker dialog
        self.hdd_dialog.update(ctx);
        if let Some(path) = self.hdd_dialog.take_picked() {
            match self.hdd_dialog.mode() {
                DialogMode::PickFile => {
                    self.emu.scsi_attach_hdd(self.hdd_dialog_idx, &path);
                }
                DialogMode::SaveFile => {
                    self.emu.scsi_branch_hdd(self.hdd_dialog_idx, &path);
                }
                _ => unreachable!(),
            }

            self.settings.fd_hdd = self.hdd_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.hdd_dialog.state() != egui_file_dialog::DialogState::Open;

        // CD-ROM image picker dialog
        self.cdrom_dialog.update(ctx);
        if let Some(path) = self.cdrom_dialog.take_picked() {
            self.emu.scsi_load_cdrom(self.cdrom_dialog_idx, &path);
            self.settings.add_recent_cd_image(&path);

            self.settings.fd_cdrom = self.cdrom_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.cdrom_dialog.state() != egui_file_dialog::DialogState::Open;

        // CD-ROM image creation dialog
        self.cdrom_files_dialog.update(ctx);
        if let Some(paths) = self.cdrom_files_dialog.take_picked_multiple() {
            match Self::create_temp_iso(&paths) {
                Ok(isofn) => {
                    log::info!("Created temporary ISO {}", isofn.display());
                    self.emu.scsi_load_cdrom(self.cdrom_dialog_idx, &isofn);
                    self.temp_files.push(isofn);
                }
                Err(e) => {
                    self.show_error(&format!("Error creating image: {:?}", e));
                }
            }

            self.settings.fd_cdrom_files = self.cdrom_files_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.cdrom_files_dialog.state() != egui_file_dialog::DialogState::Open;

        // Shared directory picker dialog
        self.shared_dir_dialog.update(ctx);
        if let Some(path) = self.shared_dir_dialog.take_picked() {
            self.workspace.set_shared_dir(Some(&path));
            self.emu.set_shared_dir(Some(path));

            self.settings.fd_shared_dir = self.shared_dir_dialog.storage_mut().clone();
            self.settings.save();
        }

        self.ui_active &= self.shared_dir_dialog.state() != egui_file_dialog::DialogState::Open;

        // Workspace picker dialog
        self.workspace_dialog.update(ctx);
        if let Some(mut path) = self.workspace_dialog.take_picked() {
            if path.exists() && !path.is_file() {
                self.show_error(&format!(
                    "Selected path is not a file: {}",
                    path.to_string_lossy()
                ));
            } else {
                self.workspace_file = Some(path.clone());
                self.workspace_dialog.config_mut().initial_directory =
                    path.parent().unwrap().to_owned();
                self.workspace_dialog.config_mut().default_file_name =
                    path.file_name().unwrap().to_string_lossy().to_string();

                if self.workspace_dialog.mode() == egui_file_dialog::DialogMode::SaveFile {
                    // 'Save workspace' / 'Save workspace as...'

                    // Add the extension if the user neglected to add the correct one.
                    // Also see https://github.com/fluxxcode/egui-file-dialog/issues/138
                    if !path
                        .extension()
                        .unwrap_or_default()
                        .eq_ignore_ascii_case("snoww")
                    {
                        let mut osstr = path.into_os_string();
                        osstr.push(".snoww");
                        path = osstr.into();
                    }
                    self.save_workspace(&path);
                    self.settings.add_recent_workspace(&path);
                } else {
                    // 'Load workspace...'
                    self.load_workspace(Some(&path));
                }

                self.update_titlebar(ctx);
            }

            self.settings.fd_workspace = self.workspace_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.workspace_dialog.state() != egui_file_dialog::DialogState::Open;

        // Record input dialog
        self.record_dialog.update(ctx);
        if let Some(path) = self.record_dialog.take_picked() {
            if path.exists() && !path.is_file() {
                self.show_error(&format!(
                    "Selected path is not a file: {}",
                    path.to_string_lossy()
                ));
            }
            if self.record_dialog.mode() == egui_file_dialog::DialogMode::SaveFile {
                self.emu.record_input(&path);
            } else if let Err(e) = self.emu.replay_input(&path) {
                self.show_error(&e);
            }

            self.settings.fd_record = self.record_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.record_dialog.state() != egui_file_dialog::DialogState::Open;

        // State file picker dialog
        let mut last = None;
        self.state_dialog
            .update_with_right_panel_ui(ctx, &mut |ui, dia| {
                if dia.selected_entry().is_some() {
                    last = dia.selected_entry().cloned();
                    if let Some(header) = &self.state_dialog_last_header {
                        let version_warning = header.snow_version.to_string() != snow_core::build_version();
                        egui::Grid::new("state_dialog_metadata").show(ui, |ui| {
                            ui.label(egui::RichText::new("Model").strong());
                            ui.label(header.model.to_string());
                            ui.end_row();
                            ui.label(egui::RichText::new("Date/time").strong());
                            ui.label(
                                header
                                    .timestamp
                                    .to_string()
                                    .parse::<chrono::DateTime<chrono::FixedOffset>>()
                                    .map(|d| d.to_rfc2822())
                                    .unwrap_or_default(),
                            );
                            ui.end_row();
                            ui.label(egui::RichText::new("Snow version").strong());
                            ui.label(egui::RichText::new(header.snow_version.to_string()).color(
                                if version_warning {
                                    egui::Color32::RED
                                } else {
                                    egui::Color32::PLACEHOLDER
                                }
                            ));
                            ui.end_row();
                        });
                        if version_warning {
                            egui::Frame::none().fill(egui::Color32::ORANGE)
                                .inner_margin(egui::Margin::same(10.0))
                                .outer_margin(egui::Margin::same(5.0))
                                .stroke(egui::Stroke::new(2.0, egui::Color32::RED))
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new("This save state is created by a different version of Snow.\n\nThis is incompatible and unsupported.\nExpect problems!").strong().color(egui::Color32::BLACK));
                                });
                        }
                        ui.separator();
                        ui.add(
                            egui::Image::from_texture(&self.state_dialog_screenshot)
                                .max_width(250.0),
                        );
                    }
                }
            });
        if last.clone().map(|d| d.to_path_buf())
            != self.state_dialog_last.clone().map(|d| d.to_path_buf())
        {
            let _ = self.state_dialog_side_update(ctx, last);
        }
        if let Some(path) = self.state_dialog.take_picked() {
            if self.state_dialog.mode() == egui_file_dialog::DialogMode::SaveFile {
                self.emu
                    .save_state(&path, self.framebuffer.screenshot().ok());
            } else {
                self.load_statefile(path);
            }

            self.settings.fd_state = self.state_dialog.storage_mut().clone();
            self.settings.save();
        }
        self.ui_active &= self.state_dialog.state() != egui_file_dialog::DialogState::Open;

        // Actual UI
        let mut central_panel = egui::CentralPanel::default();
        if self.is_ui_hidden() {
            // Remove margins from the window edges
            central_panel = central_panel.frame(egui::Frame::default().inner_margin(0.0));
        }
        central_panel.show(ctx, |ui| {
            if !self.ui_active {
                // Deactivate UI if a modal is showing
                ui.disable();
            }

            if !self.is_ui_hidden() {
                self.draw_menubar(ctx, ui);
                ui.separator();
                self.draw_toolbar(ctx, ui);
                ui.separator();
            }

            // Framebuffer display
            let response = if !self.is_ui_hidden()
                && self.workspace.framebuffer_mode == FramebufferMode::Detached
            {
                // Render framebuffer in a floating window
                egui::InnerResponse {
                    inner: (),
                    response: ui.allocate_response(ui.available_size(), egui::Sense::hover()),
                }
            } else {
                // Render framebuffer inline on the background
                ui.vertical_centered(|ui| {
                    // Align framebuffer vertically
                    if self.is_ui_hidden() {
                        const GUEST_ASPECT_RATIO: f32 = 4.0 / 3.0;
                        let host_aspect_ratio = ui.available_width() / ui.available_height();

                        if host_aspect_ratio < GUEST_ASPECT_RATIO {
                            let screen_height = 3.0 * ui.available_width() / 4.0;
                            let padding_height = (ui.available_height() - screen_height) / 2.0;

                            if padding_height > 0.0 {
                                ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                            }
                        }
                    } else if self.workspace.framebuffer_mode == FramebufferMode::Centered {
                        let padding_height =
                            (ui.available_height() - self.framebuffer.max_height()) / 2.0;
                        if padding_height > 0.0 {
                            ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                        }
                    }

                    self.framebuffer.draw(ui, self.is_ui_hidden());
                    if self.is_ui_hidden() {
                        // To fill the screen with hitbox for the context menu
                        ui.allocate_space(ui.available_size());
                    }
                })
            };
            if self.is_ui_hidden() {
                response.response.context_menu(|ui| {
                    // Show the mouse cursor so the user can interact with the menu
                    self.ui_active = false;

                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    if self.in_fullscreen && ui.button("Exit fullscreen").clicked() {
                        self.exit_fullscreen(ctx);
                        ui.close_menu();
                    }
                    if self.in_zen_mode && ui.button("Exit Zen mode").clicked() {
                        self.exit_zen_mode();
                        ui.close_menu();
                    }
                    if ui.button("Take screenshot").clicked() {
                        self.screenshot();
                        ui.close_menu();
                    }
                    ui.separator();
                    self.draw_menu_floppies(ui);
                    let targets = self.emu.get_scsi_target_status().map(|d| d.to_owned());
                    if let Some(targets) = targets {
                        for (id, target) in targets
                            .iter()
                            .enumerate()
                            .filter_map(|(i, t)| t.as_ref().map(|t| (i, t)))
                            .filter(|(_, t)| t.target_type == ScsiTargetType::Cdrom)
                        {
                            self.draw_scsi_target_menu(ui, id, Some(target), false);
                        }
                    }
                    ui.separator();
                    let mut ff = self.emu.is_fastforward();
                    if ui.checkbox(&mut ff, "Fast-forward").clicked() {
                        self.emu.toggle_fastforward();
                        ui.close_menu();
                    }
                    if ui.button("Reset machine").clicked() {
                        self.emu.reset();
                    }
                });
            }

            // Draw snowflakes behind the dialogs
            self.draw_snowflakes(ui);

            // Debugger views
            if self.emu.is_initialized() && !self.is_ui_hidden() {
                persistent_window!(self, "Disassembly")
                    .resizable([true, true])
                    .open(&mut self.workspace.disassembly_open)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            self.disassembly
                                .draw(ui, &self.emu, self.workspace.disassembly_labels);
                        });
                    });

                persistent_window_s!(self, "Registers", [300.0, 1000.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.registers_open)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            self.registers
                                .draw(ui, self.emu.get_model().unwrap().cpu_type());
                        });
                    });
                if let Some((reg, value)) = self.registers.take_edited_register() {
                    self.emu.write_register(reg, value);
                }

                persistent_window_s!(self, "Breakpoints", [300.0, 200.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.breakpoints_open)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            self.breakpoints.draw(ui, &self.emu);
                        });
                    });
                if let Some(a) = self.breakpoints.take_added_bp() {
                    self.emu.toggle_breakpoint(a);
                }

                persistent_window_s!(self, "Memory", [300.0, 200.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.memory_open)
                    .show(ctx, |ui| {
                        self.memory.draw(ui);
                    });
                if let Some((addr, value)) = self.memory.take_edited() {
                    self.emu.write_bus(addr, value);
                }

                persistent_window_s!(self, "Watchpoints", [800.0, 300.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.watchpoints_open)
                    .show(ctx, |ui| {
                        self.watchpoints
                            .draw(ui, self.memory.get_memory(), self.emu.get_cycles());
                    });
                if let Some(edited_value) = self.watchpoints.take_edited() {
                    for (offset, &byte) in edited_value.data.iter().enumerate() {
                        self.emu
                            .write_bus(edited_value.address.wrapping_add(offset as Address), byte);
                    }
                }

                persistent_window_s!(self, "Instruction history", [800.0, 300.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.instruction_history_open)
                    .show(ctx, |ui| {
                        self.instruction_history.draw(ui, self.emu.get_history());
                    });
                if self.workspace.instruction_history_open != self.emu.is_history_enabled() {
                    self.emu
                        .enable_history(self.workspace.instruction_history_open)
                        .unwrap();
                }

                persistent_window_s!(self, "System trap history", [800.0, 300.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.systrap_history_open)
                    .show(ctx, |ui| {
                        self.systrap_history
                            .draw(ui, self.emu.get_systrap_history());
                    });
                if self.workspace.systrap_history_open != self.emu.is_systrap_history_enabled() {
                    self.emu
                        .enable_systrap_history(self.workspace.systrap_history_open)
                        .unwrap();
                }

                persistent_window_s!(self, "Peripherals", [400.0, 800.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.peripheral_debug_open)
                    .show(ctx, |ui| {
                        PeripheralsWidget::new().draw(ui, self.emu.get_peripheral_debug());
                    });
                if self.workspace.peripheral_debug_open != self.emu.is_peripheral_debug_enabled() {
                    self.emu
                        .enable_peripheral_debug(self.workspace.peripheral_debug_open)
                        .unwrap();
                }

                persistent_window_s!(self, "Terminal - Channel A (modem)", [600.0, 300.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.terminal_open[0])
                    .show(ctx, |ui| {
                        self.terminal[0].draw(ui);
                    });
                if let Some(data) = self.terminal[0].pop_tx() {
                    self.emu
                        .scc_push_rx(snow_core::mac::scc::SccCh::A, data)
                        .unwrap();
                }
                if let Some(data) = self.emu.scc_take_tx(snow_core::mac::scc::SccCh::A) {
                    self.terminal[0].push_rx(&data);
                }

                persistent_window_s!(self, "Terminal - Channel B (printer)", [600.0, 300.0])
                    .resizable([true, true])
                    .open(&mut self.workspace.terminal_open[1])
                    .show(ctx, |ui| {
                        self.terminal[1].draw(ui);
                    });
                if let Some(data) = self.terminal[1].pop_tx() {
                    self.emu
                        .scc_push_rx(snow_core::mac::scc::SccCh::B, data)
                        .unwrap();
                }
                if let Some(data) = self.emu.scc_take_tx(snow_core::mac::scc::SccCh::B) {
                    self.terminal[1].push_rx(&data);
                }
            }

            // Floating framebuffer window
            if !self.is_ui_hidden() && self.workspace.framebuffer_mode == FramebufferMode::Detached
            {
                persistent_window_s!(self, "Display", [800.0, 600.0])
                    .resizable(true)
                    .show(ctx, |ui| {
                        // Draw framebuffer in a vertical container to prevent window dragging on content
                        ui.vertical(|ui| {
                            let response = self.framebuffer.draw(ui, false);
                            // Consume drag events on the framebuffer to prevent window dragging
                            ui.interact(
                                response.rect,
                                ui.id().with("fb_drag_blocker"),
                                egui::Sense::drag(),
                            );
                        });
                    });
            }
        });

        // Hide mouse over framebuffer
        // When using 'on_hover_and_drag_cursor' on the widget, the cursor still shows when the
        // mouse button is down, which is why this is done here.
        if self.ui_active
            && self.emu.is_running()
            && (self.get_machine_mouse_pos(ctx).is_some()
                || (self.in_fullscreen && self.emu.is_mouse_relative()))
        {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        // Re-render as soon as possible to keep the display updating
        ctx.request_repaint();
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if ctx.wants_keyboard_input() || !self.ui_active {
            return;
        }

        for event in &raw_input.events {
            match event {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed,
                    ..
                } => {
                    if self.get_machine_mouse_pos(ctx).is_some()
                        || (self.in_fullscreen && self.emu.is_mouse_relative())
                    {
                        // Cursor is within framebuffer view area
                        self.emu.update_mouse_button(*pressed);
                    }
                }
                egui::Event::MouseMoved(rel_p) => {
                    // Event with relative motion, but 'optional' according to egui docs
                    let relpos = if self.in_fullscreen {
                        // In fullscreen mode, do not scale the mouse as the pointer cannot leave
                        // the viewport.
                        egui::Pos2 {
                            x: rel_p.x,
                            y: rel_p.y,
                        }
                    } else {
                        egui::Pos2 {
                            x: (rel_p.x / self.framebuffer.scale) * 2.0,
                            y: (rel_p.y / self.framebuffer.scale) * 2.0,
                        }
                    };

                    if let Some(abs_p) = self.get_machine_mouse_pos(ctx) {
                        // Cursor is within framebuffer view area
                        self.emu.update_mouse(Some(&abs_p), &relpos);
                    } else if self.in_fullscreen {
                        // Always send relative motion for the entire screen in
                        // fullscreen mode
                        self.emu.update_mouse(None, &relpos);
                    }
                }
                egui::Event::PointerMoved(_) => {
                    // No relative motion in this event
                    if let Some(abs_p) = self.get_machine_mouse_pos(ctx) {
                        // Cursor is within framebuffer view area
                        // No relative motion in this event
                        self.emu.update_mouse(Some(&abs_p), &egui::Pos2::default());
                    }
                }
                egui::Event::WindowFocused(false) => {
                    self.emu.release_all_inputs();
                }
                _ => (),
            }
        }
    }
}

impl Drop for SnowGui {
    fn drop(&mut self) {
        for f in &self.temp_files {
            if !f.exists() {
                continue;
            }

            if let Err(e) = std::fs::remove_file(f) {
                log::error!("Cannot delete temp file {}: {:?}", f.display(), e);
            } else {
                log::info!("Deleted temp file {}", f.display());
            }
        }
    }
}
