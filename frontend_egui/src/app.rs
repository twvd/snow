use crate::emulator::EmulatorState;
use crate::keymap::map_winit_keycode;
use crate::widgets::framebuffer::FramebufferWidget;
use eframe::egui;
use egui_file_dialog::FileDialog;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use std::sync::Arc;

pub struct SnowGui {
    wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,

    framebuffer: FramebufferWidget,
    rom_dialog: FileDialog,

    emu: EmulatorState,
}

impl SnowGui {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        wev_recv: crossbeam_channel::Receiver<egui_winit::winit::event::WindowEvent>,
    ) -> Self {
        Self {
            wev_recv,
            framebuffer: FramebufferWidget::new(cc),
            rom_dialog: FileDialog::new()
                .add_file_filter(
                    "Macintosh ROM files (*.ROM)",
                    Arc::new(|p| p.extension().unwrap_or_default() == "rom"),
                )
                .default_file_filter("Macintosh ROM files (*.ROM)"),

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
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_winit_events();
        self.emu.poll();

        egui::CentralPanel::default().show(ctx, |ui| {
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
            if let Some(path) = self.rom_dialog.take_picked() {
                let rom = std::fs::read(path).unwrap();
                let recv = self
                    .emu
                    .init(&rom, MacModel::detect_from_rom(&rom).unwrap())
                    .expect("Emulator initialization failed");
                self.framebuffer.connect_receiver(recv);
            }

            self.framebuffer.draw(ui, self.emu.is_running());
        });

        self.rom_dialog.update(ctx);

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
                    self.emu.update_mouse_button(*pressed);
                }
                egui::Event::MouseMoved(_) | egui::Event::PointerMoved(_) => {
                    if let Some(mouse_pos) = ctx.pointer_latest_pos() {
                        let fbrect = self.framebuffer.rect();
                        let scale = egui::Vec2::from([
                            SCREEN_WIDTH as f32 / fbrect.width(),
                            SCREEN_HEIGHT as f32 / fbrect.height(),
                        ]);
                        let x = (mouse_pos.x - fbrect.left_top().x) * scale.x;
                        let y = (mouse_pos.y - fbrect.left_top().y) * scale.y;
                        if x < 0.0 || y < 0.0 || x > SCREEN_WIDTH as f32 || y > SCREEN_HEIGHT as f32
                        {
                            continue;
                        }

                        // Cursor is within framebuffer view area
                        self.emu.update_mouse(egui::Pos2::from([x, y]));
                    }
                }
                _ => (),
            }
        }
    }
}
