use crate::emulator::EmulatorState;
use crate::keymap::map_egui_keycode;
use crate::widgets::framebuffer::FramebufferWidget;
use eframe::egui;
use eframe::egui::{CursorIcon, Modifiers, PointerButton};
use snow_core::mac::video::{SCREEN_HEIGHT, SCREEN_WIDTH};
use snow_core::mac::MacModel;

pub struct SnowGui {
    emu: EmulatorState,
    framebuffer: FramebufferWidget,
    hide_cursor: bool,
    last_modifiers: Modifiers,
}

impl SnowGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            emu: Default::default(),
            framebuffer: FramebufferWidget::new(cc),
            hide_cursor: false,
            last_modifiers: Modifiers::default(),
        }
    }
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if ui.add(egui::Button::new("Start")).clicked() {
                let recv = self
                    .emu
                    .init(include_bytes!("../../plus3.rom"), MacModel::Plus)
                    .expect("Emulator initialization failed");
                self.framebuffer.connect_receiver(recv);
            }

            self.framebuffer.draw(ui);
        });

        if self.hide_cursor {
            ctx.set_cursor_icon(CursorIcon::None);
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
                    button: PointerButton::Primary,
                    pressed,
                    ..
                } => {
                    self.emu.update_mouse_button(*pressed);
                }
                egui::Event::MouseMoved(_) => {
                    if let Some(mouse_pos) = ctx.pointer_hover_pos() {
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
                        self.hide_cursor = true;
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
