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
use serde::{Deserialize, Serialize};
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use snow_core::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow_core::cpu_m68k::regs::{Register, RegisterFile};
use snow_core::debuggable::DebuggableProperties;
use snow_core::emulator::comm::{
    Breakpoint, EmulatorCommand, EmulatorEvent, EmulatorSpeed, FddStatus, ScsiTargetStatus,
    UserMessageType,
};
use snow_core::emulator::comm::{EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus};
use snow_core::emulator::{Emulator, MouseMode};
use snow_core::keymap::Scancode;
use snow_core::mac::scc::SccCh;
use snow_core::mac::scsi::target::ScsiTargetType;
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{ExtraROMs, MacModel, MacMonitor};
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::{Tickable, Ticks};
use snow_floppy::{Floppy, FloppyImage, FloppyType};

use crate::audio::SDLAudioSink;

pub type DisassemblyListing = Vec<DisassemblyEntry>;
pub type ScsiTargets = [ScsiTarget; 7];
pub struct ScsiTarget {
    pub target_type: Option<ScsiTargetType>,
    pub image_path: Option<PathBuf>,
}

impl From<ScsiTargetStatus> for ScsiTarget {
    fn from(value: ScsiTargetStatus) -> Self {
        Self {
            target_type: Some(value.target_type),
            image_path: value.image,
        }
    }
}

impl From<Option<ScsiTargetStatus>> for ScsiTarget {
    fn from(value: Option<ScsiTargetStatus>) -> Self {
        match value {
            None => Self {
                target_type: None,
                image_path: None,
            },
            Some(v) => v.into(),
        }
    }
}

/// Results of initializing the emulator, includes channels
pub struct EmulatorInitResult {
    pub frame_receiver: Receiver<DisplayBuffer>,
}

/// Initialization arguments for the emulator, minus filenames
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct EmulatorInitArgs {
    /// Disable audio, synchronize to video
    pub audio_disabled: bool,

    /// Selected monitor (if available)
    pub monitor: Option<MacMonitor>,

    #[serde(skip_serializing)]
    /// Deprecated; now mouse_mode
    pub mouse_disabled: Option<bool>,

    /// Mouse emulation mode
    pub mouse_mode: MouseMode,

    /// Start in fast-forward mode
    pub start_fastforward: bool,

    /// Configured RAM size or default if None
    pub ram_size: Option<usize>,

    /// Override the type of floppy drive (for all drives)
    pub override_fdd_type: Option<DriveType>,

    /// Enable PMMU (Macintosh II only)
    pub pmmu_enabled: bool,
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
    disasm_address: Address,
    disasm_code: DisassemblyListing,
    messages: VecDeque<(UserMessageType, String)>,
    pub last_images: [RefCell<Option<Box<FloppyImage>>>; 3],
    ram_update: VecDeque<(Address, Vec<u8>)>,
    record_input_path: Option<PathBuf>,
    instruction_history: Vec<HistoryEntry>,
    systrap_history: Vec<SystrapHistoryEntry>,
    peripheral_debug: DebuggableProperties,
    scc_tx: [VecDeque<u8>; 2],
    mouse_mode: MouseMode,

    // Clear these when emulator is de-initialized
    instruction_history_enabled: bool,
    peripheral_debug_enabled: bool,
    systrap_history_enabled: bool,
}

impl EmulatorState {
    #[allow(clippy::too_many_arguments)]
    pub fn init_from_rom(
        &mut self,
        filename: &Path,
        display_rom_path: Option<&Path>,
        extension_rom_path: Option<&Path>,
        scsi_targets: Option<ScsiTargets>,
        pram: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
    ) -> Result<EmulatorInitResult> {
        let rom = std::fs::read(filename)?;
        let display_rom = if let Some(filename) = display_rom_path {
            Some(std::fs::read(filename)?)
        } else {
            None
        };
        let extension_rom = if let Some(filename) = extension_rom_path {
            Some(std::fs::read(filename)?)
        } else {
            None
        };
        self.init(
            &rom,
            display_rom.as_deref(),
            extension_rom.as_deref(),
            scsi_targets,
            pram,
            args,
            model,
        )
    }

    #[allow(clippy::needless_pass_by_value)]
    #[allow(clippy::too_many_arguments)]
    fn init(
        &mut self,
        rom: &[u8],
        display_rom: Option<&[u8]>,
        extension_rom: Option<&[u8]>,
        scsi_targets: Option<ScsiTargets>,
        pram: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
    ) -> Result<EmulatorInitResult> {
        // Terminate running emulator (if any)
        self.deinit();

        self.last_rom = rom.to_vec();

        // Initialize emulator
        let model = if let Some(selected) = model {
            // Use the explicitly selected model
            selected
        } else {
            // Fall back to ROM autodetection
            MacModel::detect_from_rom(rom).ok_or_else(|| anyhow!("Unsupported ROM file"))?
        };
        // Build extra ROMs array
        let mut extra_roms = Vec::new();
        if let Some(display_rom) = display_rom {
            extra_roms.push(ExtraROMs::MDC12(display_rom));
        }
        if let Some(extension_rom) = extension_rom {
            extra_roms.push(ExtraROMs::ExtensionROM(extension_rom));
        }

        let mouse_mode = if matches!(args.mouse_disabled, Some(true)) {
            // Deprecated mouse_disabled
            MouseMode::Disabled
        } else {
            args.mouse_mode
        };
        self.mouse_mode = mouse_mode;

        let (mut emulator, frame_recv) = Emulator::new_with_extra(
            rom,
            &extra_roms,
            model,
            args.monitor,
            mouse_mode,
            args.ram_size,
            args.override_fdd_type,
            args.pmmu_enabled,
        )?;

        let cmd = emulator.create_cmd_sender();

        // Initialize audio
        if args.audio_disabled {
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

        if args.start_fastforward {
            cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;
        }

        if model.has_scsi() {
            for id in 0..7 {
                let Some(ref targets) = scsi_targets else {
                    break;
                };
                match &targets[id] {
                    ScsiTarget {
                        target_type: Some(ScsiTargetType::Disk),
                        image_path: Some(filename),
                    } => match emulator.load_hdd_image(filename, id) {
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
                    },
                    ScsiTarget {
                        target_type: Some(ScsiTargetType::Disk),
                        image_path: None,
                    } => {
                        // Invalid, ignore
                        log::error!("SCSI ID #{} is a hard drive but no image was specified", id);
                    }
                    ScsiTarget {
                        target_type: Some(ScsiTargetType::Cdrom),
                        ..
                    } => {
                        emulator.attach_cdrom(id);
                    }
                    ScsiTarget {
                        target_type: None, ..
                    } => (),
                };
            }
        }

        if let Some(pram_path) = pram {
            emulator.persist_pram(pram_path);
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

        Ok(EmulatorInitResult {
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

        self.instruction_history_enabled = false;
        self.systrap_history_enabled = false;
        self.peripheral_debug_enabled = false;
    }

    pub fn reset(&self) {
        if let Some(ref sender) = self.cmdsender {
            sender.send(EmulatorCommand::Reset).unwrap();
        }
    }

    pub fn update_mouse(&self, abs_p: Option<&egui::Pos2>, rel_p: &egui::Pos2) {
        if !self.is_running() {
            return;
        }

        let Some(ref sender) = self.cmdsender else {
            return;
        };

        match self.mouse_mode {
            MouseMode::Absolute => {
                if let Some(abs_p) = abs_p {
                    sender
                        .send(EmulatorCommand::MouseUpdateAbsolute {
                            x: abs_p.x as u16,
                            y: abs_p.y as u16,
                        })
                        .unwrap();
                }
            }
            MouseMode::RelativeHw => {
                if rel_p.x != 0.0 || rel_p.y != 0.0 {
                    sender
                        .send(EmulatorCommand::MouseUpdateRelative {
                            relx: rel_p.x as i16,
                            rely: rel_p.y as i16,
                            btn: None,
                        })
                        .unwrap();
                }
            }
            MouseMode::Disabled => (),
        };
    }

    pub fn update_mouse_button(&self, state: bool) {
        if !self.is_running() || self.mouse_mode == MouseMode::Disabled {
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
                EmulatorEvent::ScsiMediaEjected(id) => {
                    self.messages
                        .push_back((UserMessageType::Notice, format!("CD-ROM #{} ejected", id)));
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

    /// Gets a reference to the active SCSI target array.
    pub fn get_scsi_target_status(&self) -> Option<&[Option<ScsiTargetStatus>]> {
        let status = self.status.as_ref()?;
        if !status.model.has_scsi() {
            return None;
        }
        Some(&status.scsi)
    }

    /// Gets a copy of SCSI targets and loaded media
    pub fn get_scsi_targets(&self) -> Option<ScsiTargets> {
        let status = self.get_scsi_target_status()?;
        Some(core::array::from_fn(|id| status[id].clone().into()))
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
    pub fn scsi_attach_hdd(&self, id: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::ScsiAttachHdd(id, path.to_path_buf()))
            .unwrap();
    }

    /// Attaches a CD-ROM drive at the given ID
    pub fn scsi_attach_cdrom(&self, id: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender.send(EmulatorCommand::ScsiAttachCdrom(id)).unwrap();
    }

    /// Loads CD-ROM media into the specified SCSI target
    /// Attaches a CD-ROM drive if one is not there
    pub fn scsi_load_cdrom(&self, id: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        if let Some(status) = self.status.as_ref() {
            if !matches!(
                status.scsi[id],
                Some(ScsiTargetStatus {
                    target_type: ScsiTargetType::Cdrom,
                    ..
                })
            ) {
                self.scsi_attach_cdrom(id);
            }
        }
        sender
            .send(EmulatorCommand::ScsiLoadMedia(id, path.to_path_buf()))
            .unwrap();
    }

    /// Detach a target from a SCSI ID
    pub fn scsi_detach_target(&mut self, id: usize) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::DetachScsiTarget(id))
            .unwrap();
        self.messages.push_back((
            UserMessageType::Notice,
            format!(
                "SCSI device at #{} detached. System should be restarted.",
                id
            ),
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

    pub fn is_mouse_relative(&self) -> bool {
        self.mouse_mode == MouseMode::RelativeHw
    }
}
