use crate::emulator::EmulatorState;
use crate::keymap::map_egui_keycode;
use crate::widgets::framebuffer::FramebufferWidget;
use eframe::egui;
use eframe::egui::{Modifiers, PointerButton};
use egui_file_dialog::FileDialog;
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;
use std::sync::Arc;

pub struct SnowGui {
    framebuffer: FramebufferWidget,
    rom_dialog: FileDialog,

    emu: EmulatorState,
    last_modifiers: Modifiers,
}

impl SnowGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            framebuffer: FramebufferWidget::new(cc),
            rom_dialog: FileDialog::new()
                .add_file_filter(
                    "Macintosh ROM files (*.ROM)",
                    Arc::new(|p| p.extension().unwrap_or_default() == "rom"),
                )
                .default_file_filter("Macintosh ROM files (*.ROM)"),

            emu: Default::default(),
            last_modifiers: Modifiers::default(),
        }
    }
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if ui.add(egui::Button::new("Start")).clicked() {
                self.rom_dialog.pick_file();
            }
            if let Some(path) = self.rom_dialog.take_picked() {
                let rom = std::fs::read(path).unwrap();
                let recv = self
                    .emu
                    .init(&rom, MacModel::detect_from_rom(&rom).unwrap())
                    .expect("Emulator initialization failed");
                self.framebuffer.connect_receiver(recv);
            }

            self.framebuffer.draw(ui);
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
                    button: PointerButton::Primary,
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
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    repeat: false,
                    ..
                } => {
                    // TODO all the missing keys in egui::Key :(

                    if modifiers.alt != self.last_modifiers.alt {
                        self.emu.update_key(0x3A, modifiers.alt);
                    }
                    if modifiers.ctrl != self.last_modifiers.ctrl {
                        self.emu.update_key(0x36, modifiers.ctrl);
                    }
                    if modifiers.shift != self.last_modifiers.shift {
                        self.emu.update_key(0x38, modifiers.shift);
                    }
                    if modifiers.mac_cmd != self.last_modifiers.mac_cmd {
                        self.emu.update_key(0x37, modifiers.command);
                    }
                    self.last_modifiers = *modifiers;

                    if let Some(k) = map_egui_keycode(*key) {
                        self.emu.update_key(k, *pressed);
                    } else {
                        log::warn!("Unknown key {:?}", key);
                    }
                }
                _ => (),
            }
        }
    }
}
