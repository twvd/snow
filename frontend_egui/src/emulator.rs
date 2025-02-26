//! Emulator state management

use crate::audio::SDLAudioSink;
use anyhow::{anyhow, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use log::*;
use sdl2::audio::AudioDevice;
use snow_core::bus::Address;
use snow_core::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow_core::cpu_m68k::regs::RegisterFile;
use snow_core::emulator::comm::{
    EmulatorCommand, EmulatorEvent, EmulatorSpeed, FddStatus, HddStatus,
};
use snow_core::emulator::comm::{EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus};
use snow_core::emulator::Emulator;
use snow_core::keymap::Scancode;
use snow_core::mac::MacModel;
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::Tickable;
use std::path::{Path, PathBuf};
use std::thread;
use std::thread::JoinHandle;

pub type DisassemblyListing = Vec<DisassemblyEntry>;

/// Manages the state of the emulator and feeds input to the GUI
#[derive(Default)]
pub struct EmulatorState {
    last_rom: Vec<u8>,
    emuthread: Option<JoinHandle<()>>,
    cmdsender: Option<EmulatorCommandSender>,
    eventrecv: Option<EmulatorEventReceiver>,
    status: Option<EmulatorStatus>,
    audiosink: Option<AudioDevice<SDLAudioSink>>,
    audio_enabled: bool,
    disasm_address: Address,
    disasm_code: DisassemblyListing,
}

impl EmulatorState {
    pub fn new(audio_enabled: bool) -> Self {
        Self {
            audio_enabled,
            ..Default::default()
        }
    }

    pub fn init_from_rom(
        &mut self,
        filename: &Path,
        disks: Option<[Option<PathBuf>; 7]>,
    ) -> Result<Receiver<DisplayBuffer>> {
        let rom = std::fs::read(filename)?;
        self.init(&rom, disks)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn init(
        &mut self,
        rom: &[u8],
        disks: Option<[Option<PathBuf>; 7]>,
    ) -> Result<Receiver<DisplayBuffer>> {
        // Terminate running emulator (if any)
        self.deinit();

        self.last_rom = rom.to_vec();

        // Initialize emulator
        let model =
            MacModel::detect_from_rom(rom).ok_or_else(|| anyhow!("Unsupported ROM file"))?;
        let (mut emulator, frame_recv) = Emulator::new(rom, model)?;
        let cmd = emulator.create_cmd_sender();

        // Initialize audio
        if !self.audio_enabled {
            cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))?;
        } else if self.audiosink.is_none() {
            match SDLAudioSink::new(emulator.get_audio()) {
                Ok(sink) => self.audiosink = Some(sink),
                Err(e) => {
                    error!("Failed to initialize audio: {:?}", e);
                    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))?;
                }
            }
        } else {
            let mut cb = self.audiosink.as_mut().unwrap().lock();
            cb.set_receiver(emulator.get_audio());
        }

        if model.has_scsi() {
            for id in 0..7 {
                let Some(ref disks) = disks else {
                    break;
                };
                let Some(ref filename) = disks[id] else {
                    continue;
                };
                match emulator.load_hdd_image(filename, id) {
                    Ok(_) => {
                        info!(
                            "SCSI ID #{}: loaded image file {}",
                            id,
                            filename.to_string_lossy()
                        );
                    }
                    Err(e) => {
                        error!("SCSI ID #{}: image load failed: {}", id, e);
                    }
                }
            }
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

        // Wait for emulator to produce events and then empty the event queue once to update the
        // GUI status.
        while !self.poll() {}
        while self.poll() {}

        Ok(frame_recv)
    }

    pub fn deinit(&mut self) {
        if let Some(emu_thread) = self.emuthread.take() {
            self.cmdsender
                .as_ref()
                .unwrap()
                .send(EmulatorCommand::Quit)
                .unwrap();
            emu_thread.join().unwrap();
            self.cmdsender = None;
            self.eventrecv = None;
        }
    }

    pub fn reset(&self) {
        if let Some(ref sender) = self.cmdsender {
            sender.send(EmulatorCommand::Reset).unwrap();
        }
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

    /// Polls and empties the emulator event channel. Returns `true` if events were received.
    pub fn poll(&mut self) -> bool {
        let Some(ref eventrecv) = self.eventrecv else {
            return false;
        };
        if eventrecv.is_empty() {
            return false;
        }

        while let Ok(event) = eventrecv.try_recv() {
            match event {
                EmulatorEvent::Status(s) => {
                    self.status = Some(*s);
                }
                EmulatorEvent::NextCode((address, code)) => {
                    self.disasm_address = address;
                    self.disasm_code =
                        Vec::from_iter(Disassembler::from(&mut code.into_iter(), address));
                }
            }
        }

        true
    }

    /// Stops emulator execution
    pub fn stop(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::Stop)
            .unwrap();
    }

    /// Resumes emulator execution
    pub fn run(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::Run)
            .unwrap();
    }

    /// Executes one CPU step
    pub fn step(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::Step)
            .unwrap();
    }

    /// Returns a reference to floppy drive status for the requested drive.
    pub fn get_fdd_status(&self, drive: usize) -> Option<&FddStatus> {
        let status = self.status.as_ref()?;
        if status.fdd.len() < drive {
            return None;
        }
        if !status.fdd[drive].present {
            return None;
        }
        Some(&status.fdd[drive])
    }

    /// Gets a reference to the active SCSI hard drive array.
    pub fn get_hdds(&self) -> Option<&[Option<HddStatus>]> {
        let status = self.status.as_ref()?;
        if !status.model.has_scsi() {
            return None;
        }
        Some(&status.hdd)
    }

    /// Gets an array of PathBuf of the loaded disk images
    pub fn get_disk_paths(&self) -> [Option<PathBuf>; 7] {
        let Some(status) = self.status.as_ref() else {
            return core::array::from_fn(|_| None);
        };
        core::array::from_fn(|i| status.hdd[i].clone().map(|v| v.image))
    }

    /// Returns `true` if the emulator has been instansiated and loaded with a ROM.
    pub fn is_initialized(&self) -> bool {
        self.cmdsender.is_some()
    }

    /// Returns `true` if the emulator is running (executing)
    pub fn is_running(&self) -> bool {
        if let Some(ref status) = self.status {
            status.running
        } else {
            false
        }
    }

    /// Loads a floppy image from the specified path.
    pub fn load_floppy(&self, driveidx: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppy(
                driveidx,
                path.to_string_lossy().to_string(),
            ))
            .unwrap();
    }

    /// Loads a SCSI HDD image from the specified path.
    pub fn load_hdd_image(&self, idx: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::LoadHddImage(idx, path.to_path_buf()))
            .unwrap();
    }

    /// Detach a HDD image from a SCSI ID
    pub fn detach_hdd(&self, id: usize) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::DetachHddImage(id))
            .unwrap();
    }

    /// Returns `true` if emulator in fast-forward mode.
    pub fn is_fastforward(&self) -> bool {
        let Some(ref status) = self.status else {
            return false;
        };
        status.speed == EmulatorSpeed::Uncapped
    }

    /// Toggles emulator fast-forward mode.
    pub fn toggle_fastforward(&self) {
        let Some(ref status) = self.status else {
            return;
        };
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        if status.speed == EmulatorSpeed::Uncapped {
            let newspeed = if self.audiosink.is_some() {
                EmulatorSpeed::Accurate
            } else {
                EmulatorSpeed::Video
            };
            sender.send(EmulatorCommand::SetSpeed(newspeed)).unwrap();
        } else {
            sender
                .send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))
                .unwrap();
        }
    }

    /// Returns the currently emulated Macintosh model
    pub fn get_model(&self) -> Option<MacModel> {
        let status = self.status.as_ref()?;
        Some(status.model)
    }

    /// Returns current disassembly listing
    pub fn get_disassembly(&self) -> &DisassemblyListing {
        &self.disasm_code
    }

    /// Returns program counter register
    pub fn get_pc(&self) -> Option<Address> {
        let status = self.status.as_ref()?;
        Some(status.regs.pc)
    }

    /// Returns a reference to current register file.
    /// Panics if emulator not initialized.
    pub fn get_regs(&self) -> &RegisterFile {
        &self.status.as_ref().unwrap().regs
    }

    pub fn progkey(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::ProgKey)
            .unwrap();
    }

    pub fn get_breakpoints(&self) -> &[Address] {
        let Some(ref status) = self.status else {
            return &[];
        };
        &status.breakpoints
    }

    pub fn toggle_breakpoint(&self, addr: Address) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::ToggleBreakpoint(addr))
            .unwrap();
    }
}
