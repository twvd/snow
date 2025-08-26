use crate::dialogs::about::AboutDialog;
use crate::dialogs::diskimage::{DiskImageDialog, DiskImageDialogResult};
use crate::keymap::map_winit_keycode;
use crate::settings::AppSettings;
use crate::uniform::{UniformAction, UNIFORM_ACTION};
use crate::widgets::breakpoints::BreakpointsWidget;
use crate::widgets::disassembly::Disassembly;
use crate::widgets::framebuffer::{FramebufferWidget, ScalingAlgorithm};
use crate::widgets::instruction_history::InstructionHistoryWidget;
use crate::widgets::memory::MemoryViewerWidget;
use crate::widgets::peripherals::PeripheralsWidget;
use crate::widgets::systrap_history::SystrapHistoryWidget;
use crate::widgets::terminal::TerminalWidget;
use crate::widgets::watchpoints::WatchpointsWidget;
use crate::workspace::Workspace;
use crate::{emulator::EmulatorState, version_string, widgets::registers::RegistersWidget};
use snow_core::bus::Address;
use snow_core::mac::scsi::target::ScsiTargetType;
use snow_core::mac::MacModel;
use snow_floppy::loaders::{FloppyImageLoader, FloppyImageSaver, ImageType};

use crate::dialogs::modelselect::{ModelSelectionDialog, ModelSelectionResult};
use crate::emulator::{EmulatorInitArgs, ScsiTargets};
use anyhow::{bail, Context, Result};
use eframe::egui;
use egui_file_dialog::{DialogMode, DirectoryEntry, FileDialog};
use egui_toast::{Toast, ToastKind, ToastOptions};
use itertools::Itertools;
use rand::Rng;
use snow_core::emulator::comm::UserMessageType;
use snow_floppy::{Floppy, FloppyImage, FloppyType, OriginalTrackType};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs};
use strum::IntoEnumIterator;

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

    /// Temporary files that need cleanup on exit
    temp_files: Vec<PathBuf>,
}

impl SnowGui {
    const TOAST_DURATION: Duration = Duration::from_secs(3);
    const ZOOM_FACTORS: [f32; 8] = [0.5, 0.8, 1.0, 1.2, 1.5, 2.0, 3.0, 4.0];
    const SUBMENU_WIDTH: f32 = 175.0;

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,
        initial_file: Option<String>,
        zoom_factor: f32,
        fullscreen: bool,
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

        let mut app = Self {
            workspace: Default::default(),
            workspace_file: None,
            load_windows: false,
            first_draw: true,
            in_fullscreen: false,

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
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir()),
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
                .initial_directory(Self::default_dir()),
            cdrom_dialog_idx: 0,
            cdrom_files_dialog: FileDialog::new()
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir()),
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
                .initial_directory(Self::default_dir()),
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
                .initial_directory(Self::default_dir()),
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
                .initial_directory(Self::default_dir()),
            create_disk_dialog: Default::default(),
            model_dialog: Default::default(),
            about_dialog: AboutDialog::new(&cc.egui_ctx),
            error_dialog_open: false,
            error_string: String::new(),
            ui_active: true,
            last_running: false,

            // Snowflakes
            snowflakes: Vec::new(),
            last_snowflake_time: Instant::now(),
            snowflake_spawn_timer: 0.0,

            settings: AppSettings::load(),
            emu: EmulatorState::default(),

            temp_files: vec![],
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
            }
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
                if ui.button("Load ROM").clicked() {
                    self.model_dialog.open(
                        self.settings.get_last_roms(),
                        self.settings.get_last_display_roms(),
                    );
                    ui.close_menu();
                }
                if self.emu.is_initialized() {
                    ui.separator();

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
                    },
                );
            });
            ui.menu_button("Tools", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
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
                ui.add(
                    egui::Slider::new(&mut self.framebuffer.scale, 0.5..=4.0).text("Display scale"),
                );
                ui.add(egui::Checkbox::new(
                    &mut self.workspace.center_viewport_v,
                    "Center display vertically",
                ));

                ui.separator();
                if ui
                    .checkbox(&mut self.workspace.map_cmd_ralt, "Map right ALT to Cmd")
                    .clicked()
                {
                    ui.close_menu();
                }
            });
            ui.menu_button("View", |ui| {
                ui.set_min_width(Self::SUBMENU_WIDTH);
                if ui.button("Enter fullscreen").clicked() {
                    self.enter_fullscreen(ctx);
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
                },
            );
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
                    // TODO write support on ISM
                    //if ui
                    //    .add_enabled(
                    //        self.emu.get_model().unwrap().fdd_hd(),
                    //        egui::Button::new("Insert blank 1.44MB floppy"),
                    //    )
                    //    .clicked()
                    //{
                    //    self.emu.insert_blank_floppy(i, FloppyType::Mfm144M);
                    //    ui.close_menu();
                    //}
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
                    if ui
                        .add(
                            egui::Button::new(egui_material_icons::icons::ICON_FAST_FORWARD)
                                .selected(self.emu.is_fastforward()),
                        )
                        .on_hover_text("Fast-forward execution")
                        .clicked()
                    {
                        self.emu.toggle_fastforward();
                    }
                } else if !self.emu.is_running() {
                    if ui
                        .add(egui::Button::new(
                            egui_material_icons::icons::ICON_PLAY_ARROW,
                        ))
                        .on_hover_text("Resume execution")
                        .clicked()
                    {
                        self.emu.run();
                    }
                    if ui
                        .add(egui::Button::new(
                            egui_material_icons::icons::ICON_STEP_INTO,
                        ))
                        .on_hover_text("Step into")
                        .clicked()
                    {
                        self.emu.step();
                    }
                    if ui
                        .add(egui::Button::new(
                            egui_material_icons::icons::ICON_STEP_OVER,
                        ))
                        .on_hover_text("Step over")
                        .clicked()
                    {
                        self.emu.step_over();
                    }
                    if ui
                        .add(egui::Button::new(egui_material_icons::icons::ICON_STEP_OUT))
                        .on_hover_text("Step out")
                        .clicked()
                    {
                        self.emu.step_out();
                    }
                }

                ui.separator();
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
                    version_string(),
                    wsname,
                    m,
                    if self.emu.is_running() {
                        "running"
                    } else {
                        "stopped"
                    }
                )
            } else {
                format!("Snow v{} - {}", version_string(), wsname)
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
        match self.emu.init_from_rom(
            path,
            display_rom_path,
            extension_rom_path,
            scsi_targets,
            pram_path,
            args,
            model,
        ) {
            Ok(p) => self.framebuffer.connect_receiver(p.frame_receiver),
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
        } else {
            self.emu.deinit();
        }
    }

    fn save_workspace(&mut self, path: &Path) {
        self.workspace.viewport_scale = self.framebuffer.scale;
        self.workspace.scaling_algorithm = self.framebuffer.scaling_algorithm;
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
        if let Err(e) = self.framebuffer.write_screenshot(&p) {
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
        let mut files = hadris_iso::FileInput::empty();
        for p in paths {
            if !p.as_ref().is_file() {
                log::warn!("Skipping {}: not a file", p.as_ref().display());
                continue;
            }

            files.append(hadris_iso::File {
                path: p
                    .as_ref()
                    .file_name()
                    .context("Cannot get basename")?
                    .to_string_lossy()
                    .to_string(),
                data: hadris_iso::FileData::File(p.as_ref().to_path_buf()),
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
            while let Some((addr, data)) = self.emu.take_mem_update() {
                self.memory.update_memory(addr, &data);
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
        }
        self.ui_active &= self.floppy_dialog.state() != egui_file_dialog::DialogState::Open;

        // HDD image picker dialog
        self.hdd_dialog.update(ctx);
        if let Some(path) = self.hdd_dialog.take_picked() {
            self.emu.scsi_attach_hdd(self.hdd_dialog_idx, &path);
        }
        self.ui_active &= self.hdd_dialog.state() != egui_file_dialog::DialogState::Open;

        // CD-ROM image picker dialog
        self.cdrom_dialog.update(ctx);
        if let Some(path) = self.cdrom_dialog.take_picked() {
            self.emu.scsi_load_cdrom(self.cdrom_dialog_idx, &path);
            self.settings.add_recent_cd_image(&path);
        }
        self.ui_active &= self.cdrom_dialog.state() != egui_file_dialog::DialogState::Open;

        // CD-ROM image creatiom dialog
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
        }
        self.ui_active &= self.cdrom_files_dialog.state() != egui_file_dialog::DialogState::Open;

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
        }
        self.ui_active &= self.record_dialog.state() != egui_file_dialog::DialogState::Open;

        // Actual UI
        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.ui_active {
                // Deactivate UI if a modal is showing
                ui.disable();
            }

            if !self.in_fullscreen {
                self.draw_menubar(ctx, ui);
                ui.separator();
                self.draw_toolbar(ctx, ui);
                ui.separator();
            }

            // Framebuffer display
            let response = ui.vertical_centered(|ui| {
                // Align framebuffer vertically
                if self.in_fullscreen {
                    const GUEST_ASPECT_RATIO: f32 = 4.0 / 3.0;
                    let host_aspect_ratio = ui.available_width() / ui.available_height();

                    if host_aspect_ratio < GUEST_ASPECT_RATIO {
                        let screen_height = 3.0 * ui.available_width() / 4.0;
                        let padding_height = (ui.available_height() - screen_height) / 2.0;

                        if padding_height > 0.0 {
                            ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                        }
                    }
                } else if self.workspace.center_viewport_v {
                    let padding_height =
                        (ui.available_height() - self.framebuffer.max_height()) / 2.0;
                    if padding_height > 0.0 {
                        ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                    }
                }

                self.framebuffer.draw(ui, self.in_fullscreen);
                if self.in_fullscreen {
                    // To fill the screen with hitbox for the context menu
                    ui.allocate_space(ui.available_size());
                }
            });
            if self.in_fullscreen {
                response.response.context_menu(|ui| {
                    // Show the mouse cursor so the user can interact with the menu
                    self.ui_active = false;

                    ui.set_min_width(Self::SUBMENU_WIDTH);
                    if ui.button("Exit fullscreen").clicked() {
                        self.exit_fullscreen(ctx);
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
            if self.emu.is_initialized() && !self.in_fullscreen {
                persistent_window!(self, "Disassembly")
                    .resizable([true, true])
                    .open(&mut self.workspace.disassembly_open)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            Disassembly::new().draw(ui, &self.emu);
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
                _ => (),
            }
        }
    }
}

impl Drop for SnowGui {
    fn drop(&mut self) {
        for f in &self.temp_files {
            if let Err(e) = std::fs::remove_file(f) {
                log::error!("Cannot delete temp file {}: {:?}", f.display(), e);
            } else {
                log::info!("Deleted temp file {}", f.display());
            }
        }
    }
}
