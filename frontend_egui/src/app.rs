use crate::keymap::map_winit_keycode;
use crate::widgets::breakpoints::BreakpointsWidget;
use crate::widgets::disassembly::Disassembly;
use crate::widgets::framebuffer::FramebufferWidget;
use crate::workspace::Workspace;
use crate::{emulator::EmulatorState, widgets::registers::RegistersWidget};
use eframe::egui;
use egui_file_dialog::FileDialog;
use itertools::Itertools;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct SnowGui {
    workspace: Workspace,
    workspace_file: Option<PathBuf>,

    wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,

    framebuffer: FramebufferWidget,
    registers: RegistersWidget,
    breakpoints: BreakpointsWidget,

    rom_dialog: FileDialog,
    hdd_dialog: FileDialog,
    workspace_dialog: FileDialog,
    hdd_dialog_idx: usize,
    floppy_dialog: FileDialog,
    floppy_dialog_driveidx: usize,
    error_dialog_open: bool,
    error_string: String,
    ui_active: bool,
    last_running: bool,

    emu: EmulatorState,
}

impl SnowGui {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,
        initial_rom_file: Option<String>,
        audio_enabled: bool,
    ) -> Self {
        egui_material_icons::initialize(&cc.egui_ctx);

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

            wev_recv,
            framebuffer: FramebufferWidget::new(cc),
            registers: RegistersWidget::new(),
            breakpoints: BreakpointsWidget::new(),

            rom_dialog: FileDialog::new()
                .add_file_filter(
                    "Macintosh ROM files (*.ROM)",
                    Arc::new(|p| p.extension().unwrap_or_default() == "rom"),
                )
                .default_file_filter("Macintosh ROM files (*.ROM)"),
            hdd_dialog: FileDialog::new()
                .add_file_filter(
                    "HDD images (*.IMG)",
                    Arc::new(|p| p.extension().unwrap_or_default() == "img"),
                )
                .default_file_filter("Macintosh ROM files (*.ROM)"),
            hdd_dialog_idx: 0,
            floppy_dialog: FileDialog::new()
                .add_file_filter(
                    &floppy_filter_str,
                    Arc::new(|p| {
                        let ext = p
                            .extension()
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                            .to_string_lossy()
                            .to_string();

                        snow_floppy::loaders::ImageType::EXTENSIONS
                            .into_iter()
                            .any(|s| ext == s)
                    }),
                )
                .default_file_filter(&floppy_filter_str),
            floppy_dialog_driveidx: 0,
            workspace_dialog: FileDialog::new()
                .add_file_filter(
                    "Snow workspace (*.SNOWW)",
                    Arc::new(|p| {
                        p.extension()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case("snoww")
                    }),
                )
                .default_file_filter("Snow workspace (*.SNOWW)"),
            error_dialog_open: false,
            error_string: String::new(),
            ui_active: true,
            last_running: false,

            emu: EmulatorState::new(audio_enabled),
        };

        if let Some(filename) = initial_rom_file {
            match app.emu.init_from_rom(Path::new(&filename), None) {
                Ok(recv) => app.framebuffer.connect_receiver(recv),
                Err(e) => app.show_error(&e),
            }
        }

        app
    }

    pub fn show_error(&mut self, text: &impl std::fmt::Display) {
        self.error_dialog_open = true;
        self.error_string = text.to_string();
    }

    fn poll_winit_events(&self) {
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
                    if let Some(k) = map_winit_keycode(kc) {
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
        let fbrect = self.framebuffer.rect();
        let scale = egui::Vec2::from([
            SCREEN_WIDTH as f32 / fbrect.width(),
            SCREEN_HEIGHT as f32 / fbrect.height(),
        ]);
        let x = (mouse_pos.x - fbrect.left_top().x) * scale.x;
        let y = (mouse_pos.y - fbrect.left_top().y) * scale.y;
        if x < 0.0 || y < 0.0 || x > SCREEN_WIDTH as f32 || y > SCREEN_HEIGHT as f32 {
            None
        } else {
            Some(egui::Pos2::from([x, y]))
        }
    }

    fn update_titlebar(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            if let Some(m) = self.emu.get_model() {
                format!(
                    "Snow - {} - {} ({})",
                    self.workspace_file
                        .as_ref()
                        .and_then(|v| v.file_stem())
                        .map(|v| v.to_string_lossy())
                        .unwrap_or(std::borrow::Cow::Borrowed("Untitled workspace")),
                    m,
                    if self.emu.is_running() {
                        "running"
                    } else {
                        "stopped"
                    }
                )
            } else {
                "Snow".to_string()
            },
        ));
    }

    fn load_rom_from_path(&mut self, path: &Path, disks: Option<[Option<PathBuf>; 7]>) {
        match self.emu.init_from_rom(path, disks) {
            Ok(recv) => self.framebuffer.connect_receiver(recv),
            Err(e) => self.show_error(&e),
        }
        self.workspace.set_rom_path(path);
    }

    fn load_workspace(&mut self, path: Option<&Path>) {
        if let Some(path) = path {
            match Workspace::from_file(path) {
                Ok(ws) => {
                    self.workspace = ws;
                }
                Err(e) => self.show_error(&format!("Failed to load workspace: {}", e)),
            }
        } else {
            // Clean workspace
            self.workspace = Default::default();
        }

        // Re-initialize stuff from newly loaded workspace
        self.framebuffer.scale = self.workspace.viewport_scale;
        if let Some(rompath) = self.workspace.get_rom_path() {
            self.load_rom_from_path(&rompath, Some(self.workspace.get_disk_paths()));
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
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_winit_events();
        if self.emu.poll() {
            // Change in emulator state
            if self.last_running != self.emu.is_running() {
                self.last_running = self.emu.is_running();
                self.update_titlebar(ctx);
            }
            self.registers.update_regs(self.emu.get_regs().clone());
        }

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

        // Log window
        egui::Window::new("Log")
            .open(&mut self.workspace.log_open)
            .show(ctx, |ui| {
                egui_logger::logger_ui().show(ui);
                ui.allocate_space(ui.available_size());
            });

        // ROM picker dialog
        self.rom_dialog.update(ctx);
        if let Some(path) = self.rom_dialog.take_picked() {
            self.load_rom_from_path(&path, Some(self.emu.get_disk_paths()));
            self.update_titlebar(ctx);
        }
        self.ui_active &= self.rom_dialog.state() != egui_file_dialog::DialogState::Open;

        // Floppy image picker dialog
        self.floppy_dialog.update(ctx);
        if let Some(path) = self.floppy_dialog.take_picked() {
            self.emu.load_floppy(self.floppy_dialog_driveidx, &path);
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
            self.workspace_file = Some(path.clone());
            self.workspace_dialog.config_mut().default_file_name =
                path.to_string_lossy().to_string();

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
        self.ui_active &= self.workspace_dialog.state() != egui_file_dialog::DialogState::Open;

        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.ui_active {
                ui.disable();
            }

            // Menubar
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Workspace", |ui| {
                    if ui.button("New workspace").clicked() {
                        self.load_workspace(None);
                        self.workspace_file = None;
                        self.workspace_dialog.config_mut().default_file_name = String::new();
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
                        self.rom_dialog.pick_file();
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

                        ui.separator();
                        if ui.button("Programmers key").clicked() {
                            self.emu.progkey();
                            ui.close_menu();
                        }
                    }
                });
                if self.emu.is_initialized() {
                    ui.menu_button("Drives", |ui| {
                        for (i, d) in
                            (0..3).filter_map(|i| self.emu.get_fdd_status(i).map(|d| (i, d)))
                        {
                            if ui
                                .button(format!(
                                    "Floppy #{}: {}",
                                    i + 1,
                                    if d.ejected {
                                        "(ejected)"
                                    } else {
                                        &d.image_title
                                    }
                                ))
                                .clicked()
                            {
                                self.floppy_dialog_driveidx = i;
                                self.floppy_dialog.pick_file();
                                ui.close_menu();
                            }
                        }
                        if let Some(hdd) = self.emu.get_hdds() {
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
                                        //if ui.button("Create new image...").clicked() {
                                        //    ui.close_menu();
                                        //}
                                        if ui.button("Load disk image...").clicked() {
                                            self.hdd_dialog_idx = i;
                                            self.hdd_dialog.pick_file();
                                            ui.close_menu();
                                        }
                                    });
                                }
                            }
                        }
                    });
                }
                ui.menu_button("View", |ui| {
                    ui.add(
                        egui::Slider::new(&mut self.framebuffer.scale, 0.5..=4.0)
                            .text("Display scale"),
                    );
                    ui.add(egui::Checkbox::new(
                        &mut self.workspace.center_viewport_v,
                        "Center display vertically",
                    ));

                    ui.separator();
                    if ui.button("Log").clicked() {
                        self.workspace.log_open = !self.workspace.log_open;
                        ui.close_menu();
                    }

                    ui.separator();
                    if ui.button("Disassembly").clicked() {
                        self.workspace.disassembly_open = !self.workspace.disassembly_open;
                        ui.close_menu();
                    }
                    if ui.button("Registers").clicked() {
                        self.workspace.registers_open = !self.workspace.registers_open;
                        ui.close_menu();
                    }
                    if ui.button("Breakpoints").clicked() {
                        self.workspace.breakpoints_open = !self.workspace.breakpoints_open;
                        ui.close_menu();
                    }
                });
            });

            // Toolbar
            ui.separator();
            ui.horizontal(|ui| {
                ui.style_mut().text_styles.insert(
                    egui::TextStyle::Button,
                    egui::FontId::new(24.0, eframe::epaint::FontFamily::Proportional),
                );

                if ui
                    .add(egui::Button::new(egui_material_icons::icons::ICON_MEMORY))
                    .clicked()
                {
                    self.rom_dialog.pick_file();
                }
                if self.emu.is_initialized() {
                    ui.separator();
                    if ui
                        .add(egui::Button::new(
                            egui_material_icons::icons::ICON_RESTART_ALT,
                        ))
                        .clicked()
                    {
                        self.emu.reset();
                    }

                    if self.emu.is_running() {
                        if ui
                            .add(egui::Button::new(egui_material_icons::icons::ICON_PAUSE))
                            .clicked()
                        {
                            self.emu.stop();
                        }
                        if ui
                            .add(
                                egui::Button::new(egui_material_icons::icons::ICON_FAST_FORWARD)
                                    .selected(self.emu.is_fastforward()),
                            )
                            .clicked()
                        {
                            self.emu.toggle_fastforward();
                        }
                    } else if !self.emu.is_running() {
                        if ui
                            .add(egui::Button::new(
                                egui_material_icons::icons::ICON_PLAY_ARROW,
                            ))
                            .clicked()
                        {
                            self.emu.run();
                        }
                        if ui
                            .add(egui::Button::new(egui_material_icons::icons::ICON_STEP))
                            .clicked()
                        {
                            self.emu.step();
                        }
                    }
                }
            });
            ui.separator();

            // Framebuffer display
            ui.vertical_centered(|ui| {
                let padding_height = (ui.available_height() - self.framebuffer.max_height()) / 2.0;
                if padding_height > 0.0 && self.workspace.center_viewport_v {
                    ui.allocate_space(egui::Vec2::from([1.0, padding_height]));
                }
                self.framebuffer.draw(ui);
            });

            // Debugger views
            if self.emu.is_initialized() {
                egui::Window::new("Disassembly")
                    .resizable([true, true])
                    .open(&mut self.workspace.disassembly_open)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            Disassembly::new().draw(ui, &self.emu);
                        });
                    });

                egui::Window::new("Registers")
                    .resizable([true, true])
                    .open(&mut self.workspace.registers_open)
                    .default_width(300.0)
                    .default_height(1000.0)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            self.registers.draw(ui);
                        });
                    });

                egui::Window::new("Breakpoints")
                    .resizable([true, true])
                    .open(&mut self.workspace.breakpoints_open)
                    .default_width(300.0)
                    .default_height(200.0)
                    .show(ctx, |ui| {
                        ui.horizontal_top(|ui| {
                            self.breakpoints.draw(ui, &self.emu);
                        });
                    });
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
