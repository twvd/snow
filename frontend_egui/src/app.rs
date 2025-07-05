use crate::dialogs::diskimage::{DiskImageDialog, DiskImageDialogResult};
use crate::keymap::map_winit_keycode;
use crate::uniform::{UniformAction, UNIFORM_ACTION};
use crate::widgets::breakpoints::BreakpointsWidget;
use crate::widgets::disassembly::Disassembly;
use crate::widgets::framebuffer::FramebufferWidget;
use crate::widgets::instruction_history::InstructionHistoryWidget;
use crate::widgets::memory::MemoryViewerWidget;
use crate::widgets::peripherals::PeripheralsWidget;
use crate::widgets::systrap_history::SystrapHistoryWidget;
use crate::widgets::terminal::TerminalWidget;
use crate::widgets::watchpoints::WatchpointsWidget;
use crate::workspace::Workspace;
use crate::{emulator::EmulatorState, version_string, widgets::registers::RegistersWidget};
use snow_core::bus::Address;
use snow_core::mac::MacModel;
use snow_floppy::loaders::{FloppyImageLoader, FloppyImageSaver, ImageType};

use crate::dialogs::modelselect::{ModelSelectionDialog, ModelSelectionResult};
use crate::emulator::EmulatorInitArgs;
use anyhow::{bail, Result};
use eframe::egui;
use egui_file_dialog::{DialogMode, DirectoryEntry, FileDialog};
use egui_toast::{Toast, ToastKind, ToastOptions};
use itertools::Itertools;
use snow_core::emulator::comm::UserMessageType;
use snow_floppy::{Floppy, FloppyImage, FloppyType, OriginalTrackType};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
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

    hdd_dialog: FileDialog,
    workspace_dialog: FileDialog,
    hdd_dialog_idx: usize,
    floppy_dialog: FileDialog,
    floppy_dialog_last: Option<DirectoryEntry>,
    floppy_dialog_last_image: Option<FloppyImage>,
    floppy_dialog_last_type: Option<ImageType>,
    floppy_dialog_target: FloppyDialogTarget,
    floppy_dialog_wp: bool,
    create_disk_dialog: DiskImageDialog,
    record_dialog: FileDialog,
    model_dialog: ModelSelectionDialog,

    error_dialog_open: bool,
    error_string: String,
    ui_active: bool,
    last_running: bool,

    emu: EmulatorState,
}

impl SnowGui {
    const TOAST_DURATION: Duration = Duration::from_secs(3);
    const ZOOM_FACTORS: [f32; 8] = [0.5, 0.8, 1.0, 1.2, 1.5, 2.0, 3.0, 4.0];

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,
        initial_file: Option<String>,
        zoom_factor: f32,
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
                    "HDD images (*.img)",
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("img")
                    }),
                )
                .default_file_filter("HDD images (*.img)")
                .opening_mode(egui_file_dialog::OpeningMode::LastVisitedDir)
                .initial_directory(Self::default_dir()),
            hdd_dialog_idx: 0,
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
            error_dialog_open: false,
            error_string: String::new(),
            ui_active: true,
            last_running: false,

            emu: EmulatorState::default(),
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
        self.emu.load_hdd_image(result.scsi_id, &result.filename);
        Ok(())
    }

    fn draw_menubar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("Workspace", |ui| {
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
                if ui.button("Exit").clicked() {
                    std::process::exit(0);
                }
            });
            ui.menu_button("Machine", |ui| {
                if ui.button("Load ROM").clicked() {
                    self.model_dialog.open();
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
                    self.draw_menu_floppies(ui);

                    // Needs cloning for the later borrow to call create_disk_dialog.open()
                    let hdds = self.emu.get_hdds().map(|d| d.to_owned());
                    if let Some(hdd) = hdds {
                        ui.separator();
                        for (i, disk) in hdd.iter().enumerate() {
                            if let Some(disk) = disk {
                                // Disk loaded
                                ui.menu_button(
                                    format!(
                                        "SCSI #{}: {} ({:0.2}MB)",
                                        i,
                                        disk.image
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy(),
                                        disk.capacity / 1024 / 1024
                                    ),
                                    |ui| {
                                        if ui.button("Detach").clicked() {
                                            self.emu.detach_hdd(i);
                                            ui.close_menu();
                                        }
                                    },
                                );
                            } else {
                                // No disk
                                ui.menu_button(format!("SCSI #{}: (no disk)", i), |ui| {
                                    ui.horizontal(|ui| {
                                        if ui.button("Create new image...").clicked() {
                                            self.create_disk_dialog.open(i, &self.workspace_dir());
                                            ui.close_menu();
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        if ui.button("Load disk image...").clicked() {
                                            self.hdd_dialog_idx = i;
                                            self.hdd_dialog.pick_file();
                                            ui.close_menu();
                                        }
                                    });
                                });
                            }
                        }
                    }
                });
            }
            ui.menu_button("Ports", |ui| {
                ui.menu_button(
                    format!(
                        "{} Channel A (modem)",
                        egui_material_icons::icons::ICON_CABLE
                    ),
                    |ui| {
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
                ui.menu_button("UI scale", |ui| {
                    for z in Self::ZOOM_FACTORS {
                        if ui.button(format!("{:0.2}", z)).clicked() {
                            ctx.set_zoom_factor(z);
                            ui.close_menu();
                        }
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
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.workspace.map_cmd_ralt, "Map right ALT to Cmd")
                        .clicked()
                    {
                        ui.close_menu();
                    }
                });
            });
            ui.menu_button("View", |ui| {
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
        });
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
                    ui.horizontal(|ui| {
                        if ui.button("Insert blank 400/800K floppy").clicked() {
                            self.emu.insert_blank_floppy(i, FloppyType::Mac800K);
                            ui.close_menu();
                        }
                    });
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
                    ui.horizontal(|ui| {
                        if ui.button("Load image...").clicked() {
                            self.floppy_dialog_target = FloppyDialogTarget::Drive(i);
                            self.floppy_dialog.pick_file();
                            ui.close_menu();
                        }
                    });

                    ui.horizontal(|ui| {
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
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!d.ejected && d.dirty, egui::Button::new("Save image..."))
                            .clicked()
                        {
                            self.floppy_dialog_target = FloppyDialogTarget::Drive(i);
                            self.floppy_dialog.save_file();
                            ui.close_menu();
                        }
                    });
                    ui.horizontal(|ui| {
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
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!d.ejected, egui::Button::new("Force eject"))
                            .clicked()
                        {
                            self.emu.force_eject(i);
                            ui.close_menu();
                        }
                    });
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
                self.model_dialog.open();
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
        disks: Option<[Option<PathBuf>; 7]>,
        pram_path: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
    ) {
        match self.emu.init_from_rom(
            path,
            display_rom_path,
            extension_rom_path,
            disks,
            pram_path,
            args,
            model,
        ) {
            Ok(p) => self.framebuffer.connect_receiver(p.frame_receiver),
            Err(e) => self.show_error(&format!("Failed to load ROM file: {}", e)),
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
        if let Some(rompath) = self.workspace.get_rom_path() {
            let display_rom_path = self.workspace.get_display_card_rom_path();
            let extension_rom_path = self.workspace.get_extension_rom_path();
            let disk_paths = self.workspace.get_disk_paths();
            let pram_path = self.workspace.get_pram_path();
            let init_args = self.workspace.init_args.clone();
            let model = self.workspace.model;

            self.load_rom_from_path(
                &rompath,
                display_rom_path.as_deref(),
                extension_rom_path.as_deref(),
                Some(disk_paths),
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
        self.workspace.set_disk_paths(&self.emu.get_disk_paths());
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
            Some(self.emu.get_disk_paths()),
            result.pram_path.as_deref(),
            &result.init_args,
            Some(result.model),
        );
        self.last_running = false;
    }
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_draw {
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
            self.emu.load_hdd_image(self.hdd_dialog_idx, &path);
        }
        self.ui_active &= self.hdd_dialog.state() != egui_file_dialog::DialogState::Open;

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
            ui.vertical_centered(|ui| {
                // Align framebuffer vertically
                if self.in_fullscreen {
                    const GUEST_ASPECT_RATIO: f32 = 4.0 / 3.0;
                    let host_aspect_ratio = ui.available_width() / ui.available_height();
                    if host_aspect_ratio < GUEST_ASPECT_RATIO {
                        let screen_height = 3.0 * ui.available_width() / 4.0;
                        let padding_height = (ui.available_height() - screen_height) / 2.0;
                        ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                    }
                } else {
                    let padding_height =
                        (ui.available_height() - self.framebuffer.max_height()) / 2.0;
                    if padding_height > 0.0 && self.workspace.center_viewport_v {
                        ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                    }
                }

                let response = self.framebuffer.draw(ui, self.in_fullscreen);
                if self.in_fullscreen {
                    response.context_menu(|ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Exit fullscreen").clicked() {
                                self.exit_fullscreen(ctx);
                                ui.close_menu();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Take screenshot").clicked() {
                                self.screenshot();
                                ui.close_menu();
                            }
                        });
                        ui.separator();
                        self.draw_menu_floppies(ui);
                        ui.separator();
                        ui.horizontal(|ui| {
                            let mut ff = self.emu.is_fastforward();
                            if ui.checkbox(&mut ff, "Fast-forward").clicked() {
                                self.emu.toggle_fastforward();
                                ui.close_menu();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Reset machine").clicked() {
                                self.emu.reset();
                            }
                        });
                    });
                }
            });

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
        if self.ui_active && self.emu.is_running() && self.get_machine_mouse_pos(ctx).is_some() {
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
                    if self.get_machine_mouse_pos(ctx).is_some() {
                        // Cursor is within framebuffer view area
                        self.emu.update_mouse_button(*pressed);
                    }
                }
                egui::Event::MouseMoved(_) | egui::Event::PointerMoved(_) => {
                    if let Some(mouse_pos) = self.get_machine_mouse_pos(ctx) {
                        // Cursor is within framebuffer view area
                        self.emu.update_mouse(mouse_pos);
                    }
                }
                _ => (),
            }
        }
    }
}
