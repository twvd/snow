//! Emulator state management

#[cfg(not(feature = "audio_sdl2"))]
use crate::audio_cpal::CpalAudioProvider;
#[cfg(feature = "audio_sdl2")]
use crate::audio_sdl2::SDLAudioProvider;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;
use log::*;
use num_traits::cast::ToPrimitive;
use serde::{Deserialize, Serialize};
use snow_core::bus::Address;
use snow_core::cpu_m68k::bus::InspectView;
use snow_core::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use snow_core::cpu_m68k::disassembler::{Disassembler, DisassemblyEntry};
use snow_core::cpu_m68k::regs::{Register, RegisterFile};
use snow_core::debuggable::DebuggableProperties;
use snow_core::emulator::comm::{
    Breakpoint, EmulatorCommand, EmulatorEvent, EmulatorSpeed, FddStatus, ScsiTargetStatus,
    UserMessageType,
};
use snow_core::emulator::comm::{
    EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus, MemoryNotifierRange,
};
use snow_core::emulator::devices::{Bus, DeviceConfig, DeviceModel, MachineConfig, RefusedDevice};
use snow_core::emulator::{Emulator, MouseMode};
use snow_core::keymap::Scancode;
use snow_core::mac::maclc::pds_iie::{GamePort, IieCardSettings};
use snow_core::mac::scc::SccCh;
use snow_core::mac::scsi::target::ScsiTargetType;
use snow_core::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
use snow_core::mac::swim::drive::DriveType;
use snow_core::mac::{MacModel, MacMonitor, NubusDeviceKind, PdsCardKind};
use snow_core::renderer::DisplayBuffer;
use snow_core::tickable::{Tickable, Ticks};
use snow_core::types::LatchingEvent;
use snow_floppy::loaders::FloppyImageLoader;
use snow_floppy::{Floppy, FloppyImage, FloppyType};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::{env, fs, thread};

pub type DisassemblyListing = Vec<DisassemblyEntry>;
pub type ScsiTargets = [ScsiTarget; 7];
pub struct ScsiTarget {
    pub target_type: Option<ScsiTargetType>,
}

impl From<ScsiTargetStatus> for ScsiTarget {
    fn from(value: ScsiTargetStatus) -> Self {
        Self {
            target_type: Some(value.target_type),
        }
    }
}

impl From<Option<ScsiTargetStatus>> for ScsiTarget {
    fn from(value: Option<ScsiTargetStatus>) -> Self {
        match value {
            None => Self { target_type: None },
            Some(v) => v.into(),
        }
    }
}

/// Results of initializing the emulator, includes channels
pub struct EmulatorInitResult {
    pub frame_receiver: Arc<Mutex<Option<DisplayBuffer>>>,
    /// The devices the machine couldn't use
    pub refused: Vec<RefusedDevice>,
}

/// Initialization arguments for the emulator, minus filenames
#[derive(Serialize, Deserialize, Clone, Debug)]
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

    /// Override the type of floppy drive (for all drives). In the devices of a workspace.
    #[serde(skip_serializing)]
    pub override_fdd_type: Option<DriveType>,

    /// Enable PMMU (Macintosh II only)
    pub pmmu_enabled: bool,

    /// Enable FPU (Macintosh LC only)
    pub fpu_enabled: bool,

    /// Configured video RAM size or default if None (Macintosh LC only)
    pub vram_size: Option<usize>,

    /// Card in the PDS slot (Macintosh LC family). In the devices of a workspace.
    #[serde(skip_serializing)]
    pub pds_card: PdsCardKind,

    /// Apple IIe Card settings (used if it is in the PDS slot). In the devices of a workspace.
    #[serde(skip_serializing)]
    pub iie_card_settings: IieCardSettings,

    /// Apple IIe Card controller emulation: host keys can drive the
    /// joystick (used if it is in the PDS slot; on by default)
    pub iie_joystick_emulation: bool,

    /// Selected NuBus video card (if applicable). In the devices of a workspace.
    #[serde(skip_serializing)]
    pub video_card: NubusDeviceKind,

    /// The NuBus slot it's in. In the devices of a workspace.
    #[serde(skip_serializing)]
    pub video_card_slot: u8,

    /// Overclock
    pub overclock: Option<u64>,

    /// A data cache flush also clears the instruction cache (68040)
    pub cache_flush_workaround: bool,
}

impl Default for EmulatorInitArgs {
    fn default() -> Self {
        Self {
            audio_disabled: Default::default(),
            monitor: Default::default(),
            mouse_disabled: Default::default(),
            mouse_mode: Default::default(),
            start_fastforward: Default::default(),
            ram_size: Default::default(),
            override_fdd_type: Default::default(),
            pmmu_enabled: Default::default(),
            fpu_enabled: Default::default(),
            vram_size: Default::default(),
            pds_card: Default::default(),
            iie_card_settings: Default::default(),
            iie_joystick_emulation: true,
            video_card: Default::default(),
            video_card_slot: 0x9,
            overclock: Default::default(),
            cache_flush_workaround: Default::default(),
        }
    }
}

/// Id of the memory notifier of the disassembly
pub const NOTIFIER_DISASSEMBLY: u32 = 0;
/// Id of the memory notifier of the memory viewer
pub const NOTIFIER_MEMORY: u32 = 1;
/// First id of the memory notifiers of the watchpoints
pub const NOTIFIER_WATCHPOINTS: u32 = 2;

/// Manages the state of the emulator and feeds input to the GUI
#[derive(Default)]
pub struct EmulatorState {
    last_rom: Vec<u8>,
    emuthread: Option<JoinHandle<()>>,
    cmdsender: Option<EmulatorCommandSender>,
    eventrecv: Option<EmulatorEventReceiver>,
    status: Option<EmulatorStatus>,
    #[cfg(feature = "audio_sdl2")]
    audio_provider: Option<Arc<Mutex<SDLAudioProvider>>>,
    #[cfg(not(feature = "audio_sdl2"))]
    audio_provider: Option<Arc<Mutex<CpalAudioProvider>>>,
    disasm_address: Address,
    disasm_code: DisassemblyListing,
    messages: VecDeque<(UserMessageType, String)>,
    pub last_images: [RefCell<Option<Box<FloppyImage>>>; 3],
    /// Memory notifiers as the emulator has them, or will when there is one
    memory_notifiers: HashMap<u32, MemoryNotifierRange>,
    /// Received memory notifications: id, address, contents
    memory_notifications: VecDeque<(u32, Address, Vec<Option<u8>>)>,
    /// The answer to the last memory search
    memory_search_result: Option<Option<Address>>,
    /// The answer to the last read of the guest's clipboard
    scrap_text: Option<Option<String>>,
    record_input_path: Option<PathBuf>,
    instruction_history: Vec<HistoryEntry>,
    systrap_history: Vec<SystrapHistoryEntry>,
    peripheral_debug: DebuggableProperties,
    scc_tx: [VecDeque<u8>; 2],
    mouse_mode: MouseMode,
    /// The settings of the Apple IIe Card, if the machine has one
    iie_card: Option<IieCardSettings>,
    /// The Apple IIe Card is in front
    iie_in_front: bool,

    /// The machine came from a state file. Its disks are temporary copies and it doesn't know
    /// where its cards' ROMs came from, so it can't say what to save in a workspace.
    restored_from_state: bool,

    // Clear these when emulator is de-initialized
    instruction_history_enabled: bool,
    peripheral_debug_enabled: bool,
    systrap_history_enabled: bool,
    pub debug_framebuffers: bool,

    /// Force an emulator update signal to the App even without new events available
    force_update: LatchingEvent,

    /// Serial bridge status for SCC channels (index 0 = Channel A, index 1 = Channel B)
    serial_bridge_status: [Option<SerialBridgeStatus>; 2],

    /// Currently selected audio device on the host
    host_audio_device: Option<cpal::DeviceId>,
}

impl EmulatorState {
    #[allow(clippy::too_many_arguments)]
    pub fn init_from_rom(
        &mut self,
        filename: &Path,
        extension_rom_path: Option<&Path>,
        devices: Vec<DeviceConfig>,
        pram: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
        shared_dir: Option<PathBuf>,
        custom_datetime: Option<chrono::NaiveDateTime>,
    ) -> Result<EmulatorInitResult> {
        let rom = std::fs::read(filename)?;
        let extension_rom = if let Some(filename) = extension_rom_path {
            Some(std::fs::read(filename)?)
        } else {
            None
        };
        self.init(
            &rom,
            extension_rom.as_deref(),
            devices,
            pram,
            args,
            model,
            shared_dir,
            custom_datetime,
        )
    }

    #[allow(clippy::needless_pass_by_value)]
    #[allow(clippy::too_many_arguments)]
    fn init(
        &mut self,
        rom: &[u8],
        extension_rom: Option<&[u8]>,
        mut devices: Vec<DeviceConfig>,
        pram: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
        shared_dir: Option<PathBuf>,
        custom_datetime: Option<chrono::NaiveDateTime>,
    ) -> Result<EmulatorInitResult> {
        // Terminate running emulator (if any)
        self.deinit();

        self.last_rom = rom.to_vec();
        self.restored_from_state = false;

        // Initialize emulator
        let model = if let Some(selected) = model {
            // Use the explicitly selected model
            selected
        } else {
            // Fall back to ROM autodetection
            MacModel::detect_from_rom(rom).ok_or_else(|| anyhow!("Unsupported ROM file"))?
        };

        let mouse_mode = if matches!(args.mouse_disabled, Some(true)) {
            // Deprecated mouse_disabled
            MouseMode::Disabled
        } else {
            args.mouse_mode
        };
        self.mouse_mode = mouse_mode;
        self.iie_card = (args.pds_card == PdsCardKind::AppleIIe).then_some(args.iie_card_settings);

        #[cfg(feature = "printer")]
        for config in &mut devices {
            if config.known_model() == Some(DeviceModel::LaserWriterIISC) {
                config.settings.insert(
                    "output_dir".to_owned(),
                    serde_json::json!(Self::printer_output_dir()),
                );
            }
        }

        let (mut emulator, frame_recv, refused) = Emulator::new_with_devices(
            &MachineConfig {
                rom,
                model,
                extension_rom,
                monitor: args.monitor,
                mouse_mode,
                ram_size: args.ram_size,
                pmmu_enabled: args.pmmu_enabled,
                fpu_enabled: args.fpu_enabled,
                vram_size: args.vram_size,
            },
            devices,
        )?;

        let cmd = emulator.create_cmd_sender();
        if shared_dir.is_some() {
            cmd.send(EmulatorCommand::SetSharedDir(shared_dir))?;
        }

        if args.start_fastforward {
            cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;
        }

        cmd.send(EmulatorCommand::SetOverclock(args.overclock))?;
        cmd.send(EmulatorCommand::SetCacheFlushWorkaround(
            args.cache_flush_workaround,
        ))?;

        if let Some(pram_path) = pram {
            emulator.persist_pram(pram_path);
        }

        if let Some(dt) = custom_datetime {
            emulator.set_datetime(dt);
            info!("RTC set to custom date/time: {}", dt);
        }

        self.init_finalize(
            emulator,
            frame_recv,
            cmd,
            args.audio_disabled,
            false,
            refused,
        )
    }

    /// Where the printer's pages go
    #[cfg(feature = "printer")]
    fn printer_output_dir() -> PathBuf {
        dirs::desktop_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    pub fn init_from_statefile<P: AsRef<Path>>(
        &mut self,
        path: P,
        pause: bool,
    ) -> Result<EmulatorInitResult> {
        // Terminate running emulator (if any)
        self.deinit();

        let (emulator, frame_recv) = Emulator::load_state(path, env::temp_dir())?;
        let cmd = emulator.create_cmd_sender();
        self.restored_from_state = true;
        self.init_finalize(emulator, frame_recv, cmd, false, pause, vec![])
    }

    fn init_finalize(
        &mut self,
        mut emulator: Emulator,
        frame_recv: Arc<Mutex<Option<DisplayBuffer>>>,
        cmd: EmulatorCommandSender,
        audio_disabled: bool,
        pause: bool,
        refused: Vec<RefusedDevice>,
    ) -> Result<EmulatorInitResult> {
        // Initialize audio
        if audio_disabled {
            cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))?;
        } else {
            drop(self.audio_provider.take());

            #[cfg(not(feature = "audio_sdl2"))]
            let audio_provider_result = CpalAudioProvider::new();
            #[cfg(feature = "audio_sdl2")]
            let audio_provider_result = SDLAudioProvider::new();

            match audio_provider_result {
                Ok(provider) => {
                    self.host_audio_device = cpal::default_host()
                        .default_output_device()
                        .and_then(|o| o.id().ok());
                    let provider = Arc::new(Mutex::new(provider));
                    self.audio_provider = Some(Arc::clone(&provider));
                    if let Err(e) = emulator.set_audio_provider(provider) {
                        error!("Failed to initialize audio: {:?}", e);
                        cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))?;
                    }
                }
                Err(e) => {
                    error!("Failed to initialize audio: {:?}", e);
                    cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Video))?;
                }
            }
        }

        if !pause {
            cmd.send(EmulatorCommand::Run)?;
        } else {
            cmd.send(EmulatorCommand::Stop)?;
        }

        self.eventrecv = Some(emulator.create_event_recv());

        // Spin up emulator thread
        let emuthread = thread::spawn(move || {
            loop {
                match emulator.tick(1, ()) {
                    Ok(0) => break,
                    Ok(_) => (),
                    Err(e) => panic!("Emulator error: {}", e),
                }
            }
        });

        for (&id, &range) in &self.memory_notifiers {
            cmd.send(EmulatorCommand::MemoryNotifierSet(id, range))?;
        }
        self.cmdsender = Some(cmd);
        self.emuthread = Some(emuthread);

        // Wait for emulator to produce events and then empty the event queue once to update the
        // GUI status.
        while !self.poll() {}
        while self.poll() {}

        // Signal the UI to always update state
        self.force_update.set();

        Ok(EmulatorInitResult {
            frame_receiver: frame_recv,
            refused,
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
            self.iie_in_front = false;
        }

        self.instruction_history_enabled = false;
        self.systrap_history_enabled = false;
        self.peripheral_debug_enabled = false;
        self.debug_framebuffers = false;

        self.force_update.set();
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
        use snow_core::keymap::{KeyEvent, Keymap};

        if !self.is_running() {
            return;
        }

        if let Some(ref sender) = self.cmdsender {
            if pressed {
                sender
                    .send(EmulatorCommand::KeyEvent(KeyEvent::KeyDown(
                        key,
                        Keymap::Universal,
                    )))
                    .unwrap();
            } else {
                sender
                    .send(EmulatorCommand::KeyEvent(KeyEvent::KeyUp(
                        key,
                        Keymap::Universal,
                    )))
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
            // Check if the audio device has switched. If so, replace it.
            // Do this only when there's no activity because I don't know how expensive this is
            // on every host OS.
            self.check_audio_output_changed();

            return self.force_update.get_clear();
        }

        while let Ok(event) = eventrecv.try_recv() {
            match event {
                EmulatorEvent::Status(s) => {
                    self.status = Some(*s);
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
                EmulatorEvent::MemoryNotify { id, addr, data } => {
                    if id == NOTIFIER_DISASSEMBLY {
                        let mut code = data.into_iter().map_while(|b| b);
                        self.disasm_address = addr;
                        self.disasm_code = Vec::from_iter(Disassembler::from(&mut code, addr));
                    } else {
                        self.memory_notifications.push_back((id, addr, data));
                    }
                }
                EmulatorEvent::MemorySearchResult(result) => {
                    self.memory_search_result = Some(result);
                }
                EmulatorEvent::ScrapText(text) => self.scrap_text = Some(text),
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
                EmulatorEvent::SerialBridgeStatus(ch, status) => {
                    self.serial_bridge_status[ch.to_usize().unwrap()] = status;
                }
                EmulatorEvent::IieInFront(in_front) => self.iie_in_front = in_front,
            }
        }

        true
    }

    #[cfg(feature = "audio_sdl2")]
    /// Checks if the host device has changed its default audio output device
    fn check_audio_output_changed(&mut self) {
        // Not applicable for SDL2
        return;
    }

    /// Checks if the host device has changed its default audio output device
    #[cfg(not(feature = "audio_sdl2"))]
    fn check_audio_output_changed(&mut self) {
        let Some(device_id) = &self.host_audio_device else {
            return;
        };
        let Some(default_id) = cpal::default_host()
            .default_output_device()
            .and_then(|dev| dev.id().ok())
        else {
            return;
        };
        if default_id == *device_id {
            return;
        }

        // Default device changed, re-create the provider.
        let Some(ref cmd) = self.cmdsender else {
            return;
        };

        let Ok(new_provider) = CpalAudioProvider::new() else {
            return;
        };
        log::info!(
            "Default audio device changed from {} to {}, recreating audio provider",
            device_id,
            default_id
        );
        let new_provider = Arc::new(Mutex::new(new_provider));
        let old_provider = self.audio_provider.replace(new_provider.clone());
        self.host_audio_device = Some(default_id);
        cmd.send(EmulatorCommand::ReplaceAudioProvider(new_provider))
            .unwrap();

        // Clear the audio queue so the emulator thread can actually pick up the command
        if let Some(old_provider) = old_provider {
            old_provider.lock().unwrap().clear_channels();
        }
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

    /// The machine as it is now
    pub fn buses(&self) -> &[Bus] {
        self.status.as_ref().map_or(&[], |s| &s.buses)
    }

    pub const fn restored_from_state(&self) -> bool {
        self.restored_from_state
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

        sender
            .send(EmulatorCommand::InsertFloppy(
                driveidx,
                path.to_string_lossy().to_string(),
                wp,
            ))
            .unwrap();
    }

    /// The number of 5.25" drives connected to the Apple IIe Card.
    pub fn iie_drives_525(&self) -> usize {
        self.iie_card.map_or(0, |s| s.drives_525())
    }

    /// Loads an Apple II 5.25" DSK image into one of the IIe card's
    /// drives (0 or 1).
    pub fn load_iie_disk(&self, drive: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::InsertIieDisk(drive, path.to_path_buf()))
            .unwrap();
    }

    /// Sets the joystick and buttons on the Apple IIe card's game port.
    pub fn set_iie_game_port(&self, port: GamePort) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::SetIieGamePort(port)).unwrap();
    }

    /// Ejects one of the Apple IIe card's 5.25" drives (0 or 1).
    pub fn eject_iie_disk(&self, drive: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::EjectIieDisk(drive)).unwrap();
    }

    /// Whether a UniDisk 3.5 is connected to the Apple IIe Card.
    pub fn iie_has_unidisk(&self) -> bool {
        self.iie_card.is_some_and(|s| s.has_unidisk())
    }

    /// Loads an 800K (or 400K) ProDOS-order image into the IIe card's
    /// UniDisk 3.5.
    pub fn load_iie_unidisk(&self, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::InsertIieUniDisk(path.to_path_buf()))
            .unwrap();
    }

    /// Ejects the IIe card's UniDisk 3.5.
    pub fn eject_iie_unidisk(&self) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::EjectIieUniDisk).unwrap();
    }

    /// Loads the included toolbox floppy.
    pub fn load_toolbox_floppy(&self) -> bool {
        let Some(ref sender) = self.cmdsender else {
            return false;
        };

        // Find a free floppy drive
        for (i, d) in (0..3).filter_map(|i| self.get_fdd_status(i).map(|d| (i, d))) {
            if d.present && d.ejected {
                sender
                    .send(EmulatorCommand::InsertFloppyImage(
                        i,
                        Box::new(
                            snow_floppy::loaders::Moof::load(
                                include_bytes!("../assets/BlueSCSI Toolbox.moof"),
                                None,
                            )
                            .unwrap(),
                        ),
                        true,
                    ))
                    .unwrap();
                return true;
            }
        }
        false
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
                false,
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
                false,
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

    /// Toggles automatic writeback of a floppy drive's image to its source file.
    pub fn set_floppy_writeback(&self, driveidx: usize, enabled: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::SetFloppyWriteback(driveidx, enabled))
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

    /// Loads a SCSI HDD image from the specified path into the first free SCSI slot
    pub fn scsi_attach_hdd_firstfree(&self, path: &Path) -> bool {
        let Some(targets) = self.status.as_ref().map(|s| s.scsi.as_ref()) else {
            return false;
        };

        // Find first free SCSI slot
        let Some((id, _)) = targets.iter().enumerate().find(|(_, t)| t.is_none()) else {
            return false;
        };

        self.scsi_attach_hdd(id, path);
        true
    }

    /// Branches off a SCSI HDD image to the specified path.
    pub fn scsi_branch_hdd(&self, id: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::ScsiBranchHdd(id, path.to_path_buf()))
            .unwrap();
    }

    /// Attaches a CD-ROM drive at the given ID
    pub fn scsi_attach_cdrom(&self, id: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender.send(EmulatorCommand::ScsiAttachCdrom(id)).unwrap();
    }

    /// Attaches a folder-backed ("Toolbox managed") CD-ROM drive at the given ID.
    /// The folder's images are swappable by the guest via the BlueSCSI Toolbox
    /// CD commands.
    pub fn scsi_attach_cdrom_folder(&self, id: usize, path: PathBuf) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::ScsiAttachCdromFolder(id, path))
            .unwrap();
    }

    /// Loads CD-ROM media into the specified SCSI target
    /// Attaches a CD-ROM drive if one is not there
    pub fn scsi_load_cdrom(&self, id: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        if let Some(status) = self.status.as_ref()
            && !matches!(
                status.scsi[id],
                Some(ScsiTargetStatus {
                    target_type: ScsiTargetType::Cdrom,
                    ..
                })
            )
        {
            self.scsi_attach_cdrom(id);
        }
        sender
            .send(EmulatorCommand::ScsiLoadMedia(id, path.to_path_buf()))
            .unwrap();
    }

    /// Loads CD-ROM media into the first free SCSI CD-ROM drive
    pub fn scsi_load_cdrom_firstfree(&self, path: &Path) -> bool {
        let Some(targets) = self.status.as_ref().map(|s| s.scsi.as_ref()) else {
            return false;
        };

        // Find free CD-ROM drive
        for (i, t) in targets.iter().enumerate() {
            let Some(t) = t else {
                continue;
            };
            if t.target_type == ScsiTargetType::Cdrom && t.image.is_none() {
                self.scsi_load_cdrom(i, path);
                return true;
            }
        }
        false
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
    #[allow(dead_code)]
    pub fn is_fastforward(&self) -> bool {
        let Some(ref status) = self.status else {
            return false;
        };
        matches!(
            status.speed,
            EmulatorSpeed::Uncapped | EmulatorSpeed::FastForward(_)
        )
    }

    /// Toggles emulator fast-forward mode.
    /// `limit` optionally caps the speedup: `Some(x)` uses FastForward(x), `None` is uncapped.
    #[allow(dead_code)]
    pub fn toggle_fastforward(&self, limit: Option<f64>) {
        let Some(ref status) = self.status else {
            return;
        };
        if matches!(
            status.speed,
            EmulatorSpeed::Uncapped | EmulatorSpeed::FastForward(_)
        ) {
            self.set_speed(if self.audio_provider.is_some() {
                EmulatorSpeed::Accurate
            } else {
                EmulatorSpeed::Video
            });
        } else {
            self.set_speed(if let Some(max_speed) = limit {
                EmulatorSpeed::FastForward(max_speed)
            } else {
                EmulatorSpeed::Uncapped
            });
        }
    }

    /// Returns `true` if audio output is active.
    pub fn has_audio(&self) -> bool {
        self.audio_provider.is_some()
    }

    /// Directly sets the emulator speed
    pub fn set_speed(&self, speed: EmulatorSpeed) {
        if let Some(ref sender) = self.cmdsender {
            sender.send(EmulatorCommand::SetSpeed(speed)).unwrap();
        }
    }

    pub fn effective_speed_label(&self) -> String {
        let Some(ref status) = self.status else {
            return String::new();
        };
        if status.speed == EmulatorSpeed::Accurate {
            return String::new();
        }
        format!("{:.1}x", status.effective_speed)
    }

    /// Returns the currently emulated Macintosh model
    pub fn get_model(&self) -> Option<MacModel> {
        let status = self.status.as_ref()?;
        Some(status.model)
    }

    /// Returns true if the emulated CPU has a (paged) memory management unit
    pub fn has_pmmu(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.has_pmmu)
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

    /// Sets, or with None removes, a memory notifier. This only does something
    /// if that changes it.
    pub fn set_memory_notifier(&mut self, id: u32, range: Option<MemoryNotifierRange>) {
        let cmd = match range {
            Some(range) => {
                if self.memory_notifiers.insert(id, range) == Some(range) {
                    return;
                }
                EmulatorCommand::MemoryNotifierSet(id, range)
            }
            None => {
                if self.memory_notifiers.remove(&id).is_none() {
                    return;
                }
                EmulatorCommand::MemoryNotifierRemove(id)
            }
        };
        if let Some(ref sender) = self.cmdsender {
            sender.send(cmd).unwrap();
        }
    }

    pub fn take_memory_notification(&mut self) -> Option<(u32, Address, Vec<Option<u8>>)> {
        self.memory_notifications.pop_front()
    }

    /// Writes memory, at physical addresses if `phys`, else logical ones
    pub fn write_memory(&self, addr: Address, data: Vec<u8>, phys: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::InspectWrite { addr, data, phys })
            .unwrap();
    }

    /// Asks for the text of the guest's clipboard, see take_scrap_text()
    pub fn read_scrap_text(&self) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::ReadScrapText).unwrap();
    }

    /// The answer to read_scrap_text() when it has arrived, with the text if
    /// there is any
    pub fn take_scrap_text(&mut self) -> Option<Option<String>> {
        self.scrap_text.take()
    }

    /// Asks to look for bytes from an address on, see take_memory_search_result()
    pub fn memory_search(&self, needle: Vec<u8>, start: Address, view: InspectView) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::MemorySearch {
                start,
                needle,
                view,
            })
            .unwrap();
    }

    /// The answer to memory_search() when it has arrived, with the address if found
    pub fn take_memory_search_result(&mut self) -> Option<Option<Address>> {
        self.memory_search_result.take()
    }

    /// Asks to write a range of memory to a file
    pub fn memory_export(&self, path: PathBuf, start: Address, len: usize, view: InspectView) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::MemoryExport {
                path,
                start,
                len,
                view,
            })
            .unwrap();
    }

    /// The size of the RAM, as a default for what to export
    pub fn get_ram_size(&self) -> usize {
        self.status.as_ref().map_or(0, |s| s.ram_size)
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

    /// Whether the Apple IIe Card is in front
    pub fn iie_in_front(&self) -> bool {
        self.iie_in_front
    }

    pub fn is_mouse_relative(&self) -> bool {
        self.mouse_mode == MouseMode::RelativeHw
    }

    pub fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode
    }

    pub fn set_mouse_mode(&mut self, mode: MouseMode) {
        self.mouse_mode = mode;
        if let Some(ref sender) = self.cmdsender {
            let _ = sender.send(EmulatorCommand::SetMouseMode(mode));
        }
    }

    pub fn save_state(&self, p: &Path, screenshot: Option<Vec<u8>>) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::SaveState(p.to_path_buf(), screenshot))
            .unwrap();
    }

    pub fn set_shared_dir(&self, path: Option<PathBuf>) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::SetSharedDir(path)).unwrap();
    }

    pub fn set_debug_framebuffers(&mut self, v: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::SetDebugFramebuffers(v))
            .unwrap();
        self.debug_framebuffers = v;
    }

    pub fn set_floppy_rpm_adjustment(&self, drive: usize, adjustment: i32) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::SetFloppyRpmAdjustment(drive, adjustment))
            .unwrap();
    }

    pub fn enable_serial_bridge(&self, ch: SccCh, config: SerialBridgeConfig) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        Ok(sender.send(EmulatorCommand::SerialBridgeEnable(ch, config))?)
    }

    pub fn disable_serial_bridge(&self, ch: SccCh) -> Result<()> {
        let Some(ref sender) = self.cmdsender else {
            return Ok(());
        };
        Ok(sender.send(EmulatorCommand::SerialBridgeDisable(ch))?)
    }

    pub fn get_serial_bridge_status(&self, ch: SccCh) -> Option<&SerialBridgeStatus> {
        self.serial_bridge_status[ch.to_usize().unwrap()].as_ref()
    }

    pub fn is_serial_bridge_enabled(&self, ch: SccCh) -> bool {
        self.serial_bridge_status[ch.to_usize().unwrap()].is_some()
    }

    #[cfg(feature = "ethernet")]
    pub fn scsi_attach_ethernet(&self, id: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::ScsiAttachEthernet(id))
            .unwrap();
    }

    #[cfg(feature = "printer")]
    pub fn scsi_attach_printer(&self, id: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        let output_dir = dirs::desktop_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        sender
            .send(EmulatorCommand::ScsiAttachPrinter(id, output_dir))
            .unwrap();
    }

    #[cfg(feature = "ethernet")]
    pub fn set_eth_link(&self, id: usize, link: snow_core::mac::scsi::ethernet::EthernetLinkType) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetSetLink(id, link))
            .unwrap();
    }

    #[cfg(feature = "ethernet")]
    pub fn start_ethernet_capture<P: AsRef<std::path::Path>>(&self, scsi_id: usize, filename: P) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetStartCapture(
                scsi_id,
                filename.as_ref().to_path_buf(),
            ))
            .unwrap();
    }

    #[cfg(feature = "ethernet")]
    pub fn stop_ethernet_capture(&self, scsi_id: usize) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetStopCapture(scsi_id))
            .unwrap();
    }

    pub fn release_all_inputs(&self) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender.send(EmulatorCommand::ReleaseAllInputs).unwrap();
    }

    pub fn audio_mute(&self, muted: bool) {
        if let Some(audio_provider) = &self.audio_provider {
            audio_provider.lock().unwrap().set_mute(muted);
        }
    }

    pub fn audio_is_muted(&self) -> bool {
        self.audio_provider
            .as_ref()
            .map(|p| p.lock().unwrap().is_muted())
            .unwrap_or(false)
    }

    pub fn audio_is_underrun(&self) -> bool {
        self.audio_provider
            .as_ref()
            .map(|p| p.lock().unwrap().is_underrun())
            .unwrap_or(false)
    }
}
