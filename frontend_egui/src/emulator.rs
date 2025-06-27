//! Emulator state management

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::{fs, thread};

use anyhow::{anyhow, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use log::*;
use num_traits::cast::ToPrimitive;
use sdl2::audio::AudioDevice;

use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use snow_core::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow_core::cpu_m68k::regs::{Register, RegisterFile};
use snow_core::debuggable::DebuggableProperties;
use snow_core::emulator::comm::{
    Breakpoint, EmulatorCommand, EmulatorEvent, EmulatorSpeed, FddStatus, HddStatus,
    UserMessageType,
};
use snow_core::emulator::comm::{EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus};
use snow_core::emulator::Emulator;
use snow_core::keymap::Scancode;
use snow_core::mac::scc::SccCh;
use snow_core::mac::{ExtraROMs, MacModel};
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::{Tickable, Ticks};
use snow_floppy::{Floppy, FloppyImage, FloppyType};

use crate::audio::SDLAudioSink;

pub type DisassemblyListing = Vec<DisassemblyEntry>;

pub struct EmulatorInitParams {
    pub frame_receiver: Receiver<DisplayBuffer>,
}

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
    messages: VecDeque<(UserMessageType, String)>,
    pub last_images: [RefCell<Option<Box<FloppyImage>>>; 3],
    ram_update: VecDeque<(Address, Vec<u8>)>,
    record_input_path: Option<PathBuf>,
    instruction_history: Vec<HistoryEntry>,
    instruction_history_enabled: bool,
    systrap_history: Vec<SystrapHistoryEntry>,
    systrap_history_enabled: bool,
    peripheral_debug: DebuggableProperties,
    peripheral_debug_enabled: bool,
    scc_tx: [VecDeque<u8>; 2],
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
        display_rom_path: Option<&Path>,
        disks: Option<[Option<PathBuf>; 7]>,
        selected_model: Option<MacModel>,
    ) -> Result<EmulatorInitParams> {
        let rom = std::fs::read(filename)?;
        let display_rom = if let Some(filename) = display_rom_path {
            Some(std::fs::read(filename)?)
        } else {
            None
        };
        self.init(&rom, display_rom.as_deref(), disks, selected_model)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn init(
        &mut self,
        rom: &[u8],
        display_rom: Option<&[u8]>,
        disks: Option<[Option<PathBuf>; 7]>,
        selected_model: Option<MacModel>,
    ) -> Result<EmulatorInitParams> {
        // Terminate running emulator (if any)
        self.deinit();

        self.last_rom = rom.to_vec();

        // Initialize emulator
        let model: MacModel = if let Some(model) = selected_model {
            model
        } else {
            MacModel::detect_from_rom(rom).ok_or_else(|| anyhow!("Unsupported ROM file"))?
        };
        let (mut emulator, frame_recv) = if let Some(display_rom) = display_rom {
            Emulator::new_with_extra_roms(rom, &[ExtraROMs::MDC12(display_rom)], model)
        } else {
            Emulator::new(rom, model)
        }?;

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

        Ok(EmulatorInitParams {
            frame_receiver: frame_recv,
        })
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
            self.status = None;
            self.record_input_path = None;
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
                EmulatorEvent::FloppyEjected(idx, img) => {
                    self.messages.push_back((
                        UserMessageType::Notice,
                        format!("Floppy #{} ejected ({})", idx + 1, img.get_title()),
                    ));
                    *self.last_images[idx].borrow_mut() = Some(img);
                }
                EmulatorEvent::UserMessage(t, s) => self.messages.push_back((t, s)),
                EmulatorEvent::Memory(update) => {
                    self.ram_update.push_back(update);
                }
                EmulatorEvent::RecordedInput(i) => {
                    if let Err(e) = std::fs::write(
                        self.record_input_path.take().unwrap(),
                        serde_json::to_string(&i).unwrap(),
                    ) {
                        self.messages.push_back((
                            UserMessageType::Error,
                            format!("Cannot save recording: {}", e),
                        ));
                    }
                }
                EmulatorEvent::InstructionHistory(h) => self.instruction_history = h,
                EmulatorEvent::SystrapHistory(h) => self.systrap_history = h,
                EmulatorEvent::PeripheralDebug(d) => self.peripheral_debug = d,
                EmulatorEvent::SccTransmitData(ch, data) => {
                    self.scc_tx[ch.to_usize().unwrap()].extend(&data);
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

    /// Execute step out
    pub fn step_out(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::StepOut)
            .unwrap();
    }

    /// Execute step over
    pub fn step_over(&self) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::StepOver)
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
    pub fn load_floppy(&self, driveidx: usize, path: &Path, wp: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        if wp {
            sender
                .send(EmulatorCommand::InsertFloppyWriteProtected(
                    driveidx,
                    path.to_string_lossy().to_string(),
                ))
                .unwrap();
        } else {
            sender
                .send(EmulatorCommand::InsertFloppy(
                    driveidx,
                    path.to_string_lossy().to_string(),
                ))
                .unwrap();
        }
    }

    /// Reloads last ejected floppy
    pub fn reload_floppy(&self, driveidx: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppyImage(
                driveidx,
                self.last_images[driveidx].borrow_mut().take().unwrap(),
            ))
            .unwrap();
    }

    /// Inserts a blank floppy
    pub fn insert_blank_floppy(&self, driveidx: usize, t: FloppyType) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppyImage(
                driveidx,
                Box::new(FloppyImage::new(t, "Blank")),
            ))
            .unwrap();
    }

    /// Saves a floppy image to the specified path.
    pub fn save_floppy(&self, driveidx: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::SaveFloppy(driveidx, path.to_path_buf()))
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
    pub fn detach_hdd(&mut self, id: usize) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::DetachHddImage(id))
            .unwrap();
        self.messages.push_back((
            UserMessageType::Notice,
            format!("SCSI HDD #{} detached. System should be restarted.", id),
        ));
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

    pub fn get_breakpoints(&self) -> &[Breakpoint] {
        let Some(ref status) = self.status else {
            return &[];
        };
        &status.breakpoints
    }

    pub fn toggle_breakpoint(&self, bp: Breakpoint) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::ToggleBreakpoint(bp)).unwrap();
    }

    pub fn set_breakpoint(&self, bp: Breakpoint) {
        if !self.get_breakpoints().contains(&bp) {
            self.toggle_breakpoint(bp);
        }
    }

    pub fn take_message(&mut self) -> Option<(UserMessageType, String)> {
        self.messages.pop_front()
    }

    pub fn force_eject(&self, driveidx: usize) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::EjectFloppy(driveidx))
            .unwrap();
    }

    pub fn take_mem_update(&mut self) -> Option<(Address, Vec<u8>)> {
        self.ram_update.pop_front()
    }

    pub fn write_bus(&self, addr: Address, value: u8) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::BusInspectWrite(addr, vec![value]))
            .unwrap();
    }

    pub fn get_cycles(&self) -> Ticks {
        self.status.as_ref().unwrap().cycles
    }

    pub fn write_register(&self, reg: Register, value: u32) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::WriteRegister(reg, value))
            .unwrap();
    }

    pub fn record_input(&mut self, file: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        assert!(self.record_input_path.is_none());
        sender.send(EmulatorCommand::StartRecordingInput).unwrap();
        self.record_input_path = Some(file.to_path_buf());
    }

    pub fn record_input_end(&self) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        assert!(self.record_input_path.is_some());
        sender.send(EmulatorCommand::EndRecordingInput).unwrap();
    }

    pub fn is_recording_input(&self) -> bool {
        self.record_input_path.is_some()
    }

    pub fn replay_input(&self, file: &Path) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        let recording = serde_json::from_reader(fs::File::open(file)?)?;
        sender.send(EmulatorCommand::ReplayInputRecording(recording, true))?;
        Ok(())
    }

    pub fn enable_history(&mut self, val: bool) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        self.instruction_history_enabled = val;
        sender.send(EmulatorCommand::SetInstructionHistory(val))?;
        self.instruction_history.clear();
        Ok(())
    }

    pub fn enable_systrap_history(&mut self, val: bool) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        self.systrap_history_enabled = val;
        sender.send(EmulatorCommand::SetSystrapHistory(val))?;
        self.systrap_history.clear();
        Ok(())
    }

    pub fn is_history_enabled(&self) -> bool {
        self.instruction_history_enabled
    }

    pub fn is_systrap_history_enabled(&self) -> bool {
        self.systrap_history_enabled
    }

    pub fn get_history(&self) -> &[HistoryEntry] {
        &self.instruction_history
    }

    pub fn get_systrap_history(&self) -> &[SystrapHistoryEntry] {
        &self.systrap_history
    }

    pub fn enable_peripheral_debug(&mut self, val: bool) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        self.peripheral_debug_enabled = val;
        sender.send(EmulatorCommand::SetPeripheralDebug(val))?;
        self.peripheral_debug.clear();
        Ok(())
    }

    pub fn is_peripheral_debug_enabled(&self) -> bool {
        self.peripheral_debug_enabled
    }

    pub fn get_peripheral_debug(&self) -> &DebuggableProperties {
        &self.peripheral_debug
    }

    pub fn scc_take_tx(&mut self, ch: SccCh) -> Option<Vec<u8>> {
        let chi = ch.to_usize().unwrap();
        if self.scc_tx[chi].is_empty() {
            return None;
        }
        Some(self.scc_tx[chi].drain(..).collect())
    }

    pub fn scc_push_rx(&self, ch: SccCh, data: Vec<u8>) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        Ok(sender.send(EmulatorCommand::SccReceiveData(ch, data))?)
    }
}
