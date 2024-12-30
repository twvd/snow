use eframe::egui;
use snow_core::mac::MacModel;

use crate::emulator::EmulatorState;
use crate::widgets::framebuffer::FramebufferWidget;

pub struct SnowGui {
    emu: EmulatorState,
    framebuffer: FramebufferWidget,
}

impl SnowGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            emu: Default::default(),
            framebuffer: FramebufferWidget::new(cc),
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

        // Re-render as soon as possible to keep the display updating
        ctx.request_repaint();
    }
}
