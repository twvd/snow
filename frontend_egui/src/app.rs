use anyhow::Result;
use eframe::egui;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorSpeed};
use snow_core::emulator::Emulator;
use snow_core::mac::MacModel;
use snow_core::tickable::Tickable;
use std::thread;
use std::thread::JoinHandle;

use crate::widgets::framebuffer::FramebufferWidget;

pub struct SnowGui {
    emuthread: Option<JoinHandle<()>>,
    framebuffer: FramebufferWidget,
}

impl SnowGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            emuthread: None,
            framebuffer: FramebufferWidget::new(cc),
        }
    }

    pub fn init_emulator(&mut self) -> Result<()> {
        let rom = include_bytes!("../../plus3.rom");

        // Initialize emulator
        let (mut emulator, frame_recv) = Emulator::new(rom, MacModel::Plus)?;
        let cmd = emulator.create_cmd_sender();
        // TODO audio
        cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))?;
        cmd.send(EmulatorCommand::Run)?;

        self.framebuffer.connect_receiver(frame_recv);

        // Spin up emulator thread
        let emuthread = thread::spawn(move || loop {
            match emulator.tick(1) {
                Ok(0) => break,
                Ok(_) => (),
                Err(e) => panic!("Emulator error: {}", e),
            }
        });

        self.emuthread = Some(emuthread);

        Ok(())
    }
}

impl eframe::App for SnowGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if ui.add(egui::Button::new("Start")).clicked() {
                self.init_emulator().unwrap();
            }

            self.framebuffer.draw(ui);
        });

        // Re-render as soon as possible to keep the display updating
        ctx.request_repaint();
    }
}
