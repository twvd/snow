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
#[cfg(feature = "printer")]
use snow_core::devices::PrinterConfig;
use snow_core::devices::{
    Bus, BusKind, CdromConfig, Device, DeviceConfig, DisplayHandle, FloppyImages, FloppyInfo,
    FloppyStatus, FunctionConfig, FunctionInfo, FunctionStatus, HardDiskConfig, ModelInfo, bus_id,
    device_configs, models_for,
};
#[cfg(feature = "ethernet")]
use snow_core::devices::{EthernetConfig, EthernetLinkType};
use snow_core::emulator::comm::{
    Breakpoint, EmulatorCommand, EmulatorEvent, EmulatorSpeed, GuestScreen, UserMessageType,
};
use snow_core::emulator::comm::{
    EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus, MemoryNotifierRange,
};
use snow_core::emulator::{Emulator, MachineConfig, MouseMode};
use snow_core::keymap::Scancode;
use snow_core::mac::MacModel;
use snow_core::mac::maclc::pds_iie::GamePort;
use snow_core::mac::scc::SccCh;
use snow_core::mac::serial_bridge::{SerialBridgeConfig, SerialBridgeStatus};
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
/// The folder for the pages of a printer that was given none
#[cfg(feature = "printer")]
fn default_print_dir() -> PathBuf {
    dirs::desktop_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// A slot of the SCSI bus, as the menus show it
pub struct ScsiSlot {
    /// The SCSI ID
    pub id: usize,
    pub device: Option<Device>,
    pub status: Option<FunctionStatus>,
}

impl ScsiSlot {
    /// The name its device gives for the menus, if it gives one
    #[cfg(any(feature = "ethernet", feature = "printer"))]
    pub fn name(&self) -> &str {
        self.device
            .as_ref()
            .and_then(|device| device.name.as_deref())
            .unwrap_or_default()
    }
}

/// A device of the machine, as the Devices menu shows it
pub struct DeviceSlot {
    /// The id of its bus
    pub bus: String,
    /// The id of its slot
    pub slot: String,
    /// What the menus call the device
    pub label: String,
    pub device: Device,
    pub status: FunctionStatus,
}

/// A floppy drive of the machine, as the Devices menu shows it
pub struct FloppySlot {
    /// The id of its bus
    pub bus: String,
    /// The id of its slot
    pub slot: String,
    /// What the menus call the drive
    pub label: String,
    /// What its device model accepts and can do
    pub info: FloppyInfo,
    pub status: FloppyStatus,
}

impl FloppySlot {
    /// The drive takes the Mac's floppy images, and not image files of another kind
    pub fn takes_mac_images(&self) -> bool {
        self.info.images == FloppyImages::Mac
    }
}

/// Results of initializing the emulator, includes channels
pub struct EmulatorInitResult {
    /// The displays of the machine, at least one, in their order
    pub displays: Vec<DisplayHandle>,
    /// The temporary files of a restored state, to remove when done
    pub temp_files: Vec<PathBuf>,
    /// The devices that the machine couldn't use, each with the reason, for the user
    pub refused: Vec<String>,
}

/// Initialization arguments for the emulator, minus filenames
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct EmulatorInitArgs {
    /// Disable audio, synchronize to video
    pub audio_disabled: bool,

    #[serde(skip_serializing)]
    /// Deprecated; now mouse_mode
    pub mouse_disabled: Option<bool>,

    /// Mouse emulation mode
    pub mouse_mode: MouseMode,

    /// Start in fast-forward mode
    pub start_fastforward: bool,

    /// Configured RAM size or default if None
    pub ram_size: Option<usize>,

    /// Enable PMMU (Macintosh II only)
    pub pmmu_enabled: bool,

    /// Enable FPU (Macintosh LC only)
    pub fpu_enabled: bool,

    /// Apple IIe Card controller emulation: host keys can drive the
    /// joystick (used if it is in the PDS slot; on by default)
    pub iie_joystick_emulation: bool,

    /// Overclock
    pub overclock: Option<u64>,

    /// A data cache flush also clears the instruction cache (68040)
    pub cache_flush_workaround: bool,

    /// A user-mode PFLUSH flushes the ATCs (68040)
    pub vm_pflush_workaround: bool,

    /// The serial number of the machine, for a machine whose core makes the ROM that holds it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
}

impl Default for EmulatorInitArgs {
    fn default() -> Self {
        Self {
            audio_disabled: Default::default(),
            mouse_disabled: Default::default(),
            mouse_mode: Default::default(),
            start_fastforward: Default::default(),
            ram_size: Default::default(),
            pmmu_enabled: Default::default(),
            fpu_enabled: Default::default(),
            iie_joystick_emulation: true,
            overclock: Default::default(),
            cache_flush_workaround: Default::default(),
            vm_pflush_workaround: Default::default(),
            serial_number: None,
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
    /// The image of the last disk ejected from each floppy drive, by the ids of its bus and of
    /// its slot
    pub last_images: RefCell<HashMap<(String, String), Box<FloppyImage>>>,
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
    /// The list of buses, as the emulator last sent it
    buses: Arc<Vec<Bus>>,
    /// The status last received of the device in each slot, by the ids of its bus and slot
    device_status: HashMap<(String, String), FunctionStatus>,
    /// The Apple IIe Card is in front
    iie_in_front: bool,
    /// Where the guest puts the displays, when the machine has several
    screens: Vec<GuestScreen>,

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
        devices: Vec<DeviceConfig>,
        pram: Option<&Path>,
        args: &EmulatorInitArgs,
        model: Option<MacModel>,
        shared_dir: Option<PathBuf>,
        custom_datetime: Option<chrono::NaiveDateTime>,
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
        let mouse_mode = if matches!(args.mouse_disabled, Some(true)) {
            // Deprecated mouse_disabled
            MouseMode::Disabled
        } else {
            args.mouse_mode
        };
        self.mouse_mode = mouse_mode;

        let machine = MachineConfig {
            model,
            rom,
            extension_rom,
            ram_size: args.ram_size,
            mouse_mode,
            pmmu_enabled: args.pmmu_enabled,
            fpu_enabled: args.fpu_enabled,
            serial_number: args.serial_number.as_deref(),
        };
        let (mut emulator, displays, refused) = Emulator::new(&machine, devices)?;

        let cmd = emulator.create_cmd_sender();

        if args.start_fastforward {
            cmd.send(EmulatorCommand::SetSpeed(EmulatorSpeed::Uncapped))?;
        }

        cmd.send(EmulatorCommand::SetOverclock(args.overclock))?;
        cmd.send(EmulatorCommand::SetCacheFlushWorkaround(
            args.cache_flush_workaround,
        ))?;
        cmd.send(EmulatorCommand::SetVmPflushWorkaround(
            args.vm_pflush_workaround,
        ))?;

        if let Some(pram_path) = pram {
            emulator.persist_pram(pram_path);
        }

        if let Some(dt) = custom_datetime {
            emulator.set_datetime(dt);
            info!("RTC set to custom date/time: {}", dt);
        }

        let mut result = self.init_finalize(
            emulator,
            displays,
            cmd,
            shared_dir,
            args.audio_disabled,
            false,
        )?;
        for device in refused {
            let slot = self.slot_label(&device.config.bus, &device.config.slot);
            result.refused.push(format!("{}: {}", slot, device.reason));
        }
        Ok(result)
    }

    pub fn init_from_statefile<P: AsRef<Path>>(
        &mut self,
        path: P,
        shared_dir: Option<PathBuf>,
        pause: bool,
    ) -> Result<EmulatorInitResult> {
        // Terminate running emulator (if any)
        self.deinit();

        let (emulator, displays) = Emulator::load_state(path, env::temp_dir())?;
        let cmd = emulator.create_cmd_sender();
        let temp_files = emulator.temp_files().to_vec();
        let mut result = self.init_finalize(emulator, displays, cmd, shared_dir, false, pause)?;
        result.temp_files = temp_files;
        Ok(result)
    }

    fn init_finalize(
        &mut self,
        mut emulator: Emulator,
        displays: Vec<DisplayHandle>,
        cmd: EmulatorCommandSender,
        shared_dir: Option<PathBuf>,
        audio_disabled: bool,
        pause: bool,
    ) -> Result<EmulatorInitResult> {
        self.screens.clear();

        // The folders of the host, for a new machine and for one restored from a state alike
        cmd.send(EmulatorCommand::SetSharedDir(shared_dir))?;
        #[cfg(feature = "printer")]
        cmd.send(EmulatorCommand::SetDefaultPrintDir(default_print_dir()))?;

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

        // The Lisa's mouse only reports motion
        if self.get_model().is_some_and(MacModel::is_lisa) && self.mouse_mode == MouseMode::Absolute
        {
            self.mouse_mode = MouseMode::RelativeHw;
        }

        // Signal the UI to always update state
        self.force_update.set();

        Ok(EmulatorInitResult {
            displays,
            temp_files: vec![],
            refused: vec![],
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
            self.buses = Arc::default();
            self.device_status.clear();
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

    /// `abs_p`: the display under the pointer, by its index, and where in its pixels
    pub fn update_mouse(&self, abs_p: Option<(usize, egui::Pos2)>, rel_p: &egui::Pos2) {
        if !self.is_running() {
            return;
        }

        let Some(ref sender) = self.cmdsender else {
            return;
        };

        match self.mouse_mode {
            MouseMode::Absolute => {
                if let Some((display, abs_p)) = abs_p {
                    sender
                        .send(EmulatorCommand::MouseUpdateAbsolute {
                            x: abs_p.x as u16,
                            y: abs_p.y as u16,
                            display,
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
        self.stop_dead_audio();
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
                EmulatorEvent::FloppyEjected(bus, slot, img) => {
                    self.messages.push_back((
                        UserMessageType::Notice,
                        format!(
                            "{} ejected ({})",
                            self.slot_label(&bus, &slot),
                            img.get_title()
                        ),
                    ));
                    self.last_images.borrow_mut().insert((bus, slot), img);
                }
                EmulatorEvent::Buses(buses) => {
                    // The statuses that follow are those of the new list
                    self.buses = buses;
                    self.device_status.clear();
                }
                EmulatorEvent::DeviceStatus(bus, slot, status) => {
                    match status {
                        Some(status) => self.device_status.insert((bus, slot), status),
                        None => self.device_status.remove(&(bus, slot)),
                    };
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
                EmulatorEvent::Screens(screens) => self.screens = screens,
            }
        }

        true
    }

    #[cfg(feature = "audio_sdl2")]
    /// Stops the audio streams that had an error
    fn stop_dead_audio(&mut self) {
        // Not applicable for SDL2
    }

    /// Stops the audio streams that had an error
    #[cfg(not(feature = "audio_sdl2"))]
    fn stop_dead_audio(&mut self) {
        let Some(provider) = &self.audio_provider else {
            return;
        };
        if provider.lock().unwrap().stop_dead_streams() {
            self.messages.push_back((
                UserMessageType::Error,
                "Audio output failed and is stopped. Change the default audio device to restart it."
                    .to_string(),
            ));
        }
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

    /// What the menus and the messages call a slot: the name its device gives, or the title
    /// of its bus and the id of the slot
    pub fn slot_label(&self, bus: &str, slot: &str) -> String {
        let bus_of = self.buses.iter().find(|b| b.id == bus);
        let name = bus_of
            .and_then(|b| b.slots.iter().find(|s| s.id == slot))
            .and_then(|s| s.device.as_ref())
            .and_then(|device| device.name.clone());
        let title = bus_of.map_or(bus, |b| b.title.as_str());
        name.unwrap_or_else(|| format!("{} #{}", title, slot))
    }

    /// The devices of the buses other than SCSI, which aren't floppy drives, in the order of
    /// the list of buses: for the sections of the Devices menu
    pub fn other_devices(&self) -> Vec<DeviceSlot> {
        let mut devices = vec![];
        for bus in self.buses.iter().filter(|bus| bus.kind != BusKind::Scsi) {
            for slot in &bus.slots {
                let Some(device) = &slot.device else {
                    continue;
                };
                if matches!(device.model.info().function, FunctionInfo::Floppy(_)) {
                    continue;
                }
                let Some(status) = self.device_status(&bus.id, &slot.id) else {
                    continue;
                };
                devices.push(DeviceSlot {
                    bus: bus.id.clone(),
                    slot: slot.id.clone(),
                    label: self.slot_label(&bus.id, &slot.id),
                    device: device.clone(),
                    status: status.clone(),
                });
            }
        }
        devices
    }

    /// The floppy drives of the machine, in the order of the list of buses: the Mac's own,
    /// then those on a bus that a device adds, such as the drives of the Apple IIe Card
    pub fn floppy_slots(&self) -> Vec<FloppySlot> {
        let mut drives = vec![];
        for bus in self.buses.iter() {
            for slot in &bus.slots {
                let Some(device) = &slot.device else {
                    continue;
                };
                let FunctionInfo::Floppy(info) = device.model.info().function else {
                    continue;
                };
                let Some(FunctionStatus::Floppy(status)) = self.device_status(&bus.id, &slot.id)
                else {
                    continue;
                };
                drives.push(FloppySlot {
                    bus: bus.id.clone(),
                    slot: slot.id.clone(),
                    label: self.slot_label(&bus.id, &slot.id),
                    info,
                    status: status.clone(),
                });
            }
        }
        drives
    }

    /// The status of the device in a slot, by the ids of its bus and of the slot
    pub fn device_status(&self, bus: &str, slot: &str) -> Option<&FunctionStatus> {
        self.device_status.get(&(bus.to_string(), slot.to_string()))
    }

    /// The machine, as the list of device configs it can be made from again
    pub fn devices(&self) -> Vec<DeviceConfig> {
        match self.status.as_ref() {
            Some(status) => device_configs(&self.buses, status.model),
            None => vec![],
        }
    }

    /// The slots of the SCSI bus, if the machine has one. The id of a SCSI slot is the SCSI
    /// ID, in decimal.
    pub fn scsi_slots(&self) -> Option<Vec<ScsiSlot>> {
        let bus = self.buses.iter().find(|bus| bus.id == bus_id::SCSI)?;
        Some(
            bus.slots
                .iter()
                .filter_map(|slot| {
                    Some(ScsiSlot {
                        id: slot.id.parse().ok()?,
                        device: slot.device.clone(),
                        status: self.device_status(&bus.id, &slot.id).cloned(),
                    })
                })
                .collect(),
        )
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
    pub fn load_floppy(&self, bus: &str, slot: &str, path: &Path, wp: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::LoadMedia(
                bus.to_string(),
                slot.to_string(),
                path.to_path_buf(),
                wp,
            ))
            .unwrap();
    }

    /// Sets the joystick and buttons on the Apple IIe card's game port.
    pub fn set_iie_game_port(&self, port: GamePort) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender.send(EmulatorCommand::SetIieGamePort(port)).unwrap();
    }

    /// Presses the soft power button of a Lisa
    pub fn lisa_on_off_button(&self) {
        if let Some(sender) = &self.cmdsender {
            sender.send(EmulatorCommand::LisaOnOffButton).unwrap();
        }
    }

    /// Loads the included toolbox floppy.
    pub fn load_toolbox_floppy(&self) -> bool {
        let Some(ref sender) = self.cmdsender else {
            return false;
        };

        // Find a free floppy drive
        for drive in self.floppy_slots() {
            if drive.takes_mac_images() && drive.status.ejected {
                sender
                    .send(EmulatorCommand::InsertFloppyImage(
                        drive.bus,
                        drive.slot,
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
    pub fn reload_floppy(&self, bus: &str, slot: &str) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        let key = (bus.to_string(), slot.to_string());
        let Some(image) = self.last_images.borrow_mut().remove(&key) else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppyImage(
                key.0, key.1, image, false,
            ))
            .unwrap();
    }

    /// The image of the last disk ejected from a floppy drive
    pub fn last_image(&self, bus: &str, slot: &str) -> Option<Box<FloppyImage>> {
        let key = (bus.to_string(), slot.to_string());
        self.last_images.borrow().get(&key).cloned()
    }

    /// Inserts a blank floppy
    pub fn insert_blank_floppy(&self, bus: &str, slot: &str, t: FloppyType) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppyImage(
                bus.to_string(),
                slot.to_string(),
                Box::new(FloppyImage::new(t, "Blank")),
                false,
            ))
            .unwrap();
    }

    /// Saves a floppy image to the specified path.
    pub fn save_floppy(&self, bus: &str, slot: &str, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::SaveFloppy(
                bus.to_string(),
                slot.to_string(),
                path.to_path_buf(),
            ))
            .unwrap();
    }

    /// Toggles automatic writeback of a floppy drive's image to its source file.
    pub fn set_floppy_writeback(&self, bus: &str, slot: &str, enabled: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::SetFloppyWriteback(
                bus.to_string(),
                slot.to_string(),
                enabled,
            ))
            .unwrap();
    }

    /// Adds a SCSI device, of the device model with this function that the SCSI bus takes
    fn scsi_add_device(&self, id: usize, function: FunctionInfo, config: FunctionConfig) {
        let (Some(sender), Some(status)) = (self.cmdsender.as_ref(), self.status.as_ref()) else {
            return;
        };
        let Some(info) = models_for(BusKind::Scsi, status.model).find(|i| i.function == function)
        else {
            return;
        };

        sender
            .send(EmulatorCommand::AddDevice(DeviceConfig {
                bus: bus_id::SCSI.into(),
                slot: id.to_string(),
                model: Some(info.model),
                rom: None,
                function: config,
            }))
            .unwrap();
    }

    /// Loads a SCSI HDD image from the specified path.
    pub fn scsi_attach_hdd(&self, id: usize, path: &Path) {
        self.scsi_add_device(
            id,
            FunctionInfo::HardDisk,
            FunctionConfig::HardDisk(HardDiskConfig {
                media: Some(path.to_path_buf()),
            }),
        );
    }

    /// The hard disk that a slot of a bus takes while the machine runs, if any
    fn hard_disk_model(&self, bus: &str) -> Option<&'static ModelInfo> {
        let model = self.status.as_ref()?.model;
        let kind = self.buses.iter().find(|b| b.id == bus)?.kind;
        models_for(kind, model).find(|info| info.function == FunctionInfo::HardDisk && !info.fixed)
    }

    /// Attaches a hard disk with this image to an empty slot of a bus other than SCSI
    pub fn attach_hdd(&self, bus: &str, slot: &str, path: &Path) {
        let (Some(sender), Some(info)) = (self.cmdsender.as_ref(), self.hard_disk_model(bus))
        else {
            return;
        };
        sender
            .send(EmulatorCommand::AddDevice(DeviceConfig {
                bus: bus.into(),
                slot: slot.into(),
                model: Some(info.model),
                rom: None,
                function: FunctionConfig::HardDisk(HardDiskConfig {
                    media: Some(path.to_path_buf()),
                }),
            }))
            .unwrap();
    }

    /// The empty slots of the buses other than SCSI that take a hard disk, with the device
    /// model of that disk: for the Devices menu
    pub fn empty_hard_disk_slots(&self) -> Vec<(String, String, &'static ModelInfo)> {
        let mut slots = vec![];
        for bus in self.buses.iter().filter(|bus| bus.kind != BusKind::Scsi) {
            let Some(info) = self.hard_disk_model(&bus.id) else {
                continue;
            };
            for slot in bus.slots.iter().filter(|slot| slot.device.is_none()) {
                slots.push((bus.id.clone(), slot.id.clone(), info));
            }
        }
        slots
    }

    /// Loads a SCSI HDD image from the specified path into the first free SCSI slot
    pub fn scsi_attach_hdd_firstfree(&self, path: &Path) -> bool {
        let Some(slots) = self.scsi_slots() else {
            return false;
        };

        // Find first free SCSI slot
        let Some(slot) = slots.iter().find(|slot| slot.device.is_none()) else {
            return false;
        };

        self.scsi_attach_hdd(slot.id, path);
        true
    }

    /// Makes the hard disk in a slot carry on from a copy of its image, in this file
    pub fn branch_disk(&self, bus: &str, slot: &str, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::BranchDisk(
                bus.to_string(),
                slot.to_string(),
                path.to_path_buf(),
            ))
            .unwrap();
    }

    /// Gives the hard disk in a slot, one that can't be removed while the machine runs, the
    /// image in this file
    pub fn load_hard_disk(&self, bus: &str, slot: &str, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::LoadMedia(
                bus.to_string(),
                slot.to_string(),
                path.to_path_buf(),
                false,
            ))
            .unwrap();
    }

    /// Attaches a CD-ROM drive at the given ID
    pub fn scsi_attach_cdrom(&self, id: usize) {
        self.scsi_add_device(
            id,
            FunctionInfo::Cdrom,
            FunctionConfig::Cdrom(CdromConfig::default()),
        );
    }

    /// Attaches a folder-backed ("Toolbox managed") CD-ROM drive at the given ID.
    /// The folder's images are swappable by the guest via the BlueSCSI Toolbox
    /// CD commands.
    /// Gives a folder of disc images to the CD-ROM drive at `id`, or to a new drive there if
    /// the SCSI ID is free
    pub fn scsi_attach_cdrom_folder(&self, id: usize, path: PathBuf) {
        let slot = id.to_string();
        if matches!(
            self.device_status(bus_id::SCSI, &slot),
            Some(FunctionStatus::Cdrom(_))
        ) {
            if let Some(sender) = self.cmdsender.as_ref() {
                sender
                    .send(EmulatorCommand::CdromSetFolder(
                        bus_id::SCSI.into(),
                        slot,
                        path,
                    ))
                    .unwrap();
            }
            return;
        }
        self.scsi_add_device(
            id,
            FunctionInfo::Cdrom,
            FunctionConfig::Cdrom(CdromConfig { folder: Some(path) }),
        );
    }

    /// Loads CD-ROM media into the specified SCSI target
    /// Attaches a CD-ROM drive if one is not there
    pub fn scsi_load_cdrom(&self, id: usize, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        let slot = id.to_string();
        if !matches!(
            self.device_status(bus_id::SCSI, &slot),
            Some(FunctionStatus::Cdrom(_))
        ) {
            self.scsi_attach_cdrom(id);
        }
        sender
            .send(EmulatorCommand::LoadMedia(
                bus_id::SCSI.into(),
                slot,
                path.to_path_buf(),
                false,
            ))
            .unwrap();
    }

    /// Loads CD-ROM media into the first free SCSI CD-ROM drive
    pub fn scsi_load_cdrom_firstfree(&self, path: &Path) -> bool {
        let Some(slots) = self.scsi_slots() else {
            return false;
        };

        // Find free CD-ROM drive
        for slot in slots {
            if matches!(slot.status, Some(FunctionStatus::Cdrom(cdrom)) if cdrom.image.is_none()) {
                self.scsi_load_cdrom(slot.id, path);
                return true;
            }
        }
        false
    }

    /// Detach a target from a SCSI ID
    pub fn scsi_detach_target(&mut self, id: usize) {
        self.remove_device(bus_id::SCSI, &id.to_string());
    }

    /// Takes the device in a slot out of the machine
    pub fn remove_device(&mut self, bus: &str, slot: &str) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::RemoveDevice(
                bus.to_string(),
                slot.to_string(),
            ))
            .unwrap();
        let device = if bus == bus_id::SCSI {
            format!("SCSI device at #{}", slot)
        } else {
            self.slot_label(bus, slot)
        };
        self.messages.push_back((
            UserMessageType::Notice,
            format!("{} detached. System should be restarted.", device),
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

    pub fn force_eject(&self, bus: &str, slot: &str) {
        self.cmdsender
            .as_ref()
            .unwrap()
            .send(EmulatorCommand::EjectMedia(
                bus.to_string(),
                slot.to_string(),
            ))
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

    /// Where the guest puts the displays, when the machine has several
    pub fn screens(&self) -> &[GuestScreen] {
        &self.screens
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

    pub fn set_floppy_rpm_adjustment(&self, bus: &str, slot: &str, adjustment: i32) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::SetFloppyRpmAdjustment(
                bus.to_string(),
                slot.to_string(),
                adjustment,
            ))
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

    /// Attaches an Ethernet adapter. The core gives it an Ethernet address no other adapter has.
    #[cfg(feature = "ethernet")]
    pub fn scsi_attach_ethernet(&self, id: usize) {
        let Some(status) = self.status.as_ref() else {
            return;
        };
        let Some(info) = models_for(BusKind::Scsi, status.model)
            .find(|info| matches!(info.function, FunctionInfo::Ethernet(_)))
        else {
            return;
        };
        self.scsi_add_device(
            id,
            info.function,
            FunctionConfig::Ethernet(EthernetConfig {
                mac: None,
                link: EthernetLinkType::default(),
            }),
        );
    }

    #[cfg(feature = "printer")]
    pub fn scsi_attach_printer(&self, id: usize) {
        self.scsi_add_device(
            id,
            FunctionInfo::Printer,
            FunctionConfig::Printer(PrinterConfig::default()),
        );
    }

    #[cfg(feature = "ethernet")]
    pub fn set_eth_link(&self, bus: &str, slot: &str, link: EthernetLinkType) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetSetLink(
                bus.to_string(),
                slot.to_string(),
                link,
            ))
            .unwrap();
    }

    #[cfg(feature = "ethernet")]
    pub fn start_ethernet_capture<P: AsRef<std::path::Path>>(
        &self,
        bus: &str,
        slot: &str,
        filename: P,
    ) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetStartCapture(
                bus.to_string(),
                slot.to_string(),
                filename.as_ref().to_path_buf(),
            ))
            .unwrap();
    }

    #[cfg(feature = "ethernet")]
    pub fn stop_ethernet_capture(&self, bus: &str, slot: &str) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetStopCapture(
                bus.to_string(),
                slot.to_string(),
            ))
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
