//! Emulator state management

use anyhow::Result;
use crossbeam_channel::Receiver;
use eframe::egui;
use snow_core::emulator::comm::EmulatorCommandSender;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorSpeed};
use snow_core::emulator::Emulator;
use snow_core::mac::MacModel;
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::Tickable;
use std::thread;
use std::thread::JoinHandle;

#[derive(Default)]
pub struct EmulatorState {
    emuthread: Option<JoinHandle<()>>,
    cmdsender: Option<EmulatorCommandSender>,
}

impl EmulatorState {
    pub fn init(&mut self, rom: &[u8], model: MacModel) -> Result<Receiver<DisplayBuffer>> {
        // Initialize emulator
        let (mut emulator, frame_recv) = Emulator::new(rom, model)?;
        let cmd = emulator.create_cmd_sender();
        // TODO audio
        cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;
        cmd.send(EmulatorCommand::Run)?;

        // Spin up emulator thread
        let emuthread = thread::spawn(move || loop {
            match emulator.tick(1) {
                Ok(0) => break,
                Ok(_) => (),
                Err(e) => panic!("Emulator error: {}", e),
            }
        });

        self.cmdsender = Some(cmd);
        self.emuthread = Some(emuthread);

        Ok(frame_recv)
    }

    pub fn update_mouse(&mut self, p: egui::Pos2) {
        if let Some(ref sender) = self.cmdsender {
            sender
                .send(EmulatorCommand::MouseUpdateAbsolute {
                    x: p.x as u16,
                    y: p.y as u16,
                })
                .unwrap();
        }
    }

    pub fn update_mouse_button(&mut self, state: bool) {
        if let Some(ref sender) = self.cmdsender {
            sender
                .send(EmulatorCommand::MouseUpdateRelative {
                    relx: 0,
                    rely: 0,
                    btn: Some(state),
                })
                .unwrap();
        }
    }
}
