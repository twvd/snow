//! Emulator state management

use anyhow::Result;
use crossbeam_channel::Receiver;
use eframe::egui;
use log::*;
use sdl2::audio::AudioDevice;
use snow_core::emulator::comm::{EmulatorCommand, EmulatorEvent, EmulatorSpeed};
use snow_core::emulator::comm::{EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus};
use snow_core::emulator::Emulator;
use snow_core::keymap::Scancode;
use snow_core::mac::MacModel;
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::Tickable;
use std::thread;
use std::thread::JoinHandle;

use crate::audio::SDLAudioSink;

#[derive(Default)]
pub struct EmulatorState {
    emuthread: Option<JoinHandle<()>>,
    cmdsender: Option<EmulatorCommandSender>,
    eventrecv: Option<EmulatorEventReceiver>,
    status: Option<EmulatorStatus>,
    audiosink: Option<AudioDevice<SDLAudioSink>>,
}

impl EmulatorState {
    pub fn init(&mut self, rom: &[u8], model: MacModel) -> Result<Receiver<DisplayBuffer>> {
        // Terminate running emulator (if any)
        if let Some(emu_thread) = self.emuthread.take() {
            self.cmdsender
                .as_ref()
                .unwrap()
                .send(EmulatorCommand::Quit)
                .unwrap();
            emu_thread.join().unwrap();
        }

        // Initialize emulator
        let (mut emulator, frame_recv) = Emulator::new(rom, model)?;
        let cmd = emulator.create_cmd_sender();
        if self.audiosink.is_none() {
            match SDLAudioSink::new(emulator.get_audio()) {
                Ok(sink) => self.audiosink = Some(sink),
                Err(e) => {
                    error!("Failed to initialize audio: {:?}", e);
                    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))
                        .unwrap();
                }
            }
        } else {
            let mut cb = self.audiosink.as_mut().unwrap().lock();
            cb.set_receiver(emulator.get_audio());
        }
        cmd.send(EmulatorCommand::Run)?;

        self.eventrecv = Some(emulator.create_event_recv());

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

    pub fn update_mouse(&self, p: egui::Pos2) {
        if !self.is_running() {
            return;
        }

        if let Some(ref sender) = self.cmdsender {
            sender
                .send(EmulatorCommand::MouseUpdateAbsolute {
                    x: p.x as u16,
                    y: p.y as u16,
                })
                .unwrap();
        }
    }

    pub fn update_mouse_button(&self, state: bool) {
        if !self.is_running() {
            return;
        }

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

    pub fn update_key(&self, key: Scancode, pressed: bool) {
        if !self.is_running() {
            return;
        }

        if let Some(ref sender) = self.cmdsender {
            if pressed {
                sender
                    .send(EmulatorCommand::KeyEvent(
                        snow_core::keymap::KeyEvent::KeyDown(key),
                    ))
                    .unwrap();
            } else {
                sender
                    .send(EmulatorCommand::KeyEvent(
                        snow_core::keymap::KeyEvent::KeyUp(key),
                    ))
                    .unwrap();
            }
        }
    }

    pub fn poll(&mut self) {
        let Some(ref eventrecv) = self.eventrecv else {
            return;
        };
        if eventrecv.is_empty() {
            return;
        }

        while let Ok(event) = eventrecv.try_recv() {
            match event {
                EmulatorEvent::Status(s) => {
                    self.status = Some(*s);
                }
                EmulatorEvent::NextCode(_) => {}
            }
        }
    }

    pub fn stop(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::Stop)
            .unwrap();
    }

    pub fn run(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::Run)
            .unwrap();
    }

    pub fn step(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::Step)
            .unwrap();
    }

    pub fn is_initialized(&self) -> bool {
        self.cmdsender.is_some()
    }

    pub fn is_running(&self) -> bool {
        if let Some(ref status) = self.status {
            status.running
        } else {
            false
        }
    }
}
