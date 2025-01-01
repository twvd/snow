use crate::emulator::EmulatorState;
use crate::keymap::map_winit_keycode;
use crate::widgets::framebuffer::FramebufferWidget;
use eframe::egui;
use egui_file_dialog::FileDialog;
use itertools::Itertools;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use std::sync::Arc;

pub struct SnowGui {
    wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,

    framebuffer: FramebufferWidget,
    rom_dialog: FileDialog,
    floppy_dialog: FileDialog,
    floppy_dialog_driveidx: usize,

    emu: EmulatorState,
}

impl SnowGui {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,
    ) -> Self {
        let floppy_filter_str = format!(
            "Floppy images ({})",
            snow_floppy::loaders::ImageType::EXTENSIONS
                .into_iter()
                .map(|e| format!("*.{}", e.to_ascii_uppercase()))
                .join(", ")
        );
        Self {
            wev_recv,
            framebuffer: FramebufferWidget::new(cc),
            rom_dialog: FileDialog::new()
                .add_file_filter(
                    "Macintosh ROM files (*.ROM)",
                    Arc::new(|p| p.extension().unwrap_or_default() == "rom"),
                )
                .default_file_filter("Macintosh ROM files (*.ROM)"),
            floppy_dialog: FileDialog::new().add_file_filter(
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
            ),
            floppy_dialog_driveidx: 0,

            emu: Default::default(),
        }
    }

    fn poll_winit_events(&self) {
        if !self.wev_recv.is_empty() {
            while let Ok(wevent) = self.wev_recv.try_recv() {
                use egui_winit::winit::event::{KeyEvent, WindowEvent};
                use egui_winit::winit::keyboard::PhysicalKey;

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
    }

    fn get_machine_mouse_pos(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
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
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_winit_events();
        self.emu.poll();

        egui::CentralPanel::default().show(ctx, |ui| {
            // Menubar
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Emulator", |ui| {
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
                            for (i, sz) in hdd.iter().enumerate() {
                                if ui
                                    .button(format!(
                                        "SCSI #{}: {}",
                                        i,
                                        if let Some(sz) = sz {
                                            format!("{:0.2}MB", sz / 1024 / 1024)
                                        } else {
                                            "(no disk)".to_string()
                                        }
                                    ))
                                    .clicked()
                                {
                                    ui.close_menu();
                                }
                            }
                        }
                    });
                }
            });

            // Toolbar
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Load ROM")).clicked() {
                    self.rom_dialog.pick_file();
                }
                if self.emu.is_initialized() {
                    ui.separator();

                    if self.emu.is_running() && ui.add(egui::Button::new("Stop")).clicked() {
                        self.emu.stop();
                    } else if !self.emu.is_running() && ui.add(egui::Button::new("Run")).clicked() {
                        self.emu.run();
                    }
                }
            });
            ui.separator();

            // Framebuffer display
            self.framebuffer.draw(ui, self.emu.is_running());
        });

        // ROM picker dialog
        self.rom_dialog.update(ctx);
        if let Some(path) = self.rom_dialog.take_picked() {
            let rom = std::fs::read(path).unwrap();
            let recv = self
                .emu
                .init(&rom, MacModel::detect_from_rom(&rom).unwrap())
                .expect("Emulator initialization failed");
            self.framebuffer.connect_receiver(recv);
        }

        // Floppy image picker dialog
        self.floppy_dialog.update(ctx);
        if let Some(path) = self.floppy_dialog.take_picked() {
            self.emu.load_floppy(self.floppy_dialog_driveidx, &path);
        }

        // Re-render as soon as possible to keep the display updating
        ctx.request_repaint();
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if ctx.wants_keyboard_input() {
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
