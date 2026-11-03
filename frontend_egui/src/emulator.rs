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
    Breakpoint, EmulatorCommand, EmulatorEvent, EmulatorSpeed, UserMessageType,
};
use snow_core::emulator::comm::{
    EmulatorCommandSender, EmulatorEventReceiver, EmulatorStatus, MemoryNotifierRange,
};
use snow_core::emulator::devices::{
    Bus, BusKind, CdromSettings, DeviceConfig, DeviceModel, FileSharingSettings, FunctionKind,
    FunctionStatus, HD20_SLOT, HardDiskSettings, MachineConfig, RefusedDevice, SerialLinkSettings,
    Slot, SlotAddr, StatusBlock, device_configs, models_for,
};
use snow_core::emulator::{Emulator, MouseMode};
use snow_core::keymap::Scancode;
use snow_core::mac::maclc::pds_iie::{GamePort, IieCardSettings};
use snow_core::mac::scc::SccCh;
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
    /// The disk last ejected from each floppy drive
    pub last_images: RefCell<HashMap<SlotAddr, Box<FloppyImage>>>,
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
    /// The machine's buses. They only change when a device comes or goes.
    buses: Arc<Vec<Bus>>,
    /// Where the devices' status is, and the copy the menus draw from
    status_block: Option<Arc<StatusBlock>>,
    statuses: Vec<Vec<Option<FunctionStatus>>>,
    /// The generation of the block that copy is of
    status_generation: u64,

    // Clear these when emulator is de-initialized
    instruction_history_enabled: bool,
    peripheral_debug_enabled: bool,
    systrap_history_enabled: bool,
    pub debug_framebuffers: bool,

    /// Force an emulator update signal to the App even without new events available
    force_update: LatchingEvent,

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

        for config in &mut devices {
            Self::add_host_settings(config);
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

    /// What's up to the host rather than to the machine or the workspace
    fn add_host_settings(config: &mut DeviceConfig) {
        #[cfg(feature = "printer")]
        if config.known_model() == Some(DeviceModel::LaserWriterIISC) {
            let output_dir = dirs::desktop_dir()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            config
                .settings
                .insert("output_dir".to_owned(), serde_json::json!(output_dir));
        }
        #[cfg(not(feature = "printer"))]
        let _ = config;
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
        self.buses = emulator.buses();
        self.status_block = Some(emulator.status_block());
        self.status_generation = u64::MAX;

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
            self.buses = Arc::default();
            self.status_block = None;
            self.statuses.clear();
            self.record_input_path = None;
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
        let statuses_changed = self.copy_statuses();
        let Some(ref eventrecv) = self.eventrecv else {
            return false;
        };
        if eventrecv.is_empty() {
            // Check if the audio device has switched. If so, replace it.
            // Do this only when there's no activity because I don't know how expensive this is
            // on every host OS.
            self.check_audio_output_changed();

            return self.force_update.get_clear() || statuses_changed;
        }

        while let Ok(event) = eventrecv.try_recv() {
            match event {
                EmulatorEvent::Status(s) => {
                    self.status = Some(*s);
                }
                EmulatorEvent::Buses(buses) => self.buses = buses,
                EmulatorEvent::FloppyEjected(addr, img) => {
                    self.messages.push_back((
                        UserMessageType::Notice,
                        format!("{} ejected ({})", self.slot_label(addr), img.get_title()),
                    ));
                    self.last_images.borrow_mut().insert(addr, img);
                }
                EmulatorEvent::MediaEjected(addr) => {
                    self.messages.push_back((
                        UserMessageType::Notice,
                        format!("{}: disc ejected", self.slot_label(addr)),
                    ));
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
                // The status block has it
                EmulatorEvent::SerialBridgeStatus(..) => (),
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

    /// Copies the devices' status when it has changed. The lock is only held for the copy.
    fn copy_statuses(&mut self) -> bool {
        let Some(block) = self.status_block.as_ref() else {
            return false;
        };
        let generation = block.generation();
        if generation == self.status_generation {
            return false;
        }
        self.statuses.clone_from(&block.statuses.lock().unwrap());
        self.status_generation = generation;
        true
    }

    /// The machine as it is now
    pub fn buses(&self) -> &[Bus] {
        &self.buses
    }

    /// What a device in a slot shows
    pub fn status(&self, addr: SlotAddr) -> Option<&FunctionStatus> {
        self.statuses.get(addr.bus)?.get(addr.slot)?.as_ref()
    }

    /// The list this machine can be created from, for a workspace
    pub fn devices(&self) -> Vec<DeviceConfig> {
        match self.get_model() {
            Some(model) => device_configs(&self.buses, &self.statuses, model),
            None => vec![],
        }
    }

    /// Hands a device its settings, creating it if its slot is empty
    pub fn add_device(&self, mut config: DeviceConfig) {
        Self::add_host_settings(&mut config);
        if let Some(sender) = self.cmdsender.as_ref() {
            sender.send(EmulatorCommand::AddDevice(config)).unwrap();
        }
    }

    pub fn slot(&self, addr: SlotAddr) -> Option<(&Bus, &Slot)> {
        let bus = self.buses().get(addr.bus)?;
        Some((bus, bus.slots.get(addr.slot)?))
    }

    /// How the user knows a slot: "SCSI #3"
    pub fn slot_label(&self, addr: SlotAddr) -> String {
        match self.slot(addr) {
            Some((bus, slot)) => match &slot.name {
                Some(name) => format!("{} #{} {}", bus.title, slot.id, name),
                None => format!("{} #{}", bus.title, slot.id),
            },
            None => "Device".to_owned(),
        }
    }

    /// All slots, with their addresses
    fn all_slots(&self) -> impl Iterator<Item = (SlotAddr, &Bus, &Slot)> {
        self.buses().iter().enumerate().flat_map(|(b, bus)| {
            bus.slots
                .iter()
                .enumerate()
                .map(move |(s, slot)| (SlotAddr { bus: b, slot: s }, bus, slot))
        })
    }

    /// A slot that can't take a device right now, so the menus grey it. Only the HD20 chain
    /// has those for now: an HD20 attaches at the next free position.
    pub fn slot_unavailable(&self, addr: SlotAddr) -> bool {
        let Some((bus, slot)) = self.slot(addr) else {
            return true;
        };
        let Some(position) = hd20_position(slot) else {
            return false;
        };
        let attached = bus
            .slots
            .iter()
            .filter(|s| s.device.is_some() && hd20_position(s).is_some())
            .count();
        position > attached + 1
    }

    /// A slot the menus don't show. For now, the empty HD20 slots of the Macs whose ROM
    /// doesn't look for HD20s.
    pub fn slot_hidden(&self, addr: SlotAddr) -> bool {
        let Some((_, slot)) = self.slot(addr) else {
            return true;
        };
        slot.device.is_none()
            && hd20_position(slot).is_some()
            && !matches!(
                self.get_model(),
                Some(
                    MacModel::Early128K
                        | MacModel::Early512K
                        | MacModel::Early512Ke
                        | MacModel::Plus
                )
            )
    }

    /// The device model of a function this slot's bus takes, if it can take one now
    fn offered(&self, addr: SlotAddr, function: FunctionKind) -> Option<DeviceModel> {
        let (bus, _) = self.slot(addr)?;
        if self.slot_unavailable(addr) {
            return None;
        }
        let model = self.get_model()?;
        models_for(bus.kind, model)
            .find(|info| info.function == function && bus.can_take(info))
            .map(|info| info.model)
    }

    /// An empty slot that can take a device of this function, SCSI first
    fn free_slot(&self, function: FunctionKind) -> Option<SlotAddr> {
        let free = |(addr, _, slot): &(SlotAddr, &Bus, &Slot)| {
            slot.device.is_none()
                && !self.slot_hidden(*addr)
                && self.offered(*addr, function).is_some()
        };
        self.all_slots()
            .filter(|(_, bus, _)| bus.kind == BusKind::Scsi)
            .find(free)
            .or_else(|| self.all_slots().find(free))
            .map(|(addr, _, _)| addr)
    }

    /// The first Mac floppy drive with no disk in it
    pub fn free_floppy_drive(&self) -> Option<SlotAddr> {
        self.all_slots()
            .find(|(addr, _, _)| {
                matches!(self.status(*addr), Some(FunctionStatus::Floppy(f)) if f.ejected && f.can_save)
            })
            .map(|(addr, _, _)| addr)
    }

    /// Creates a device of a model
    pub fn attach_model(
        &self,
        addr: SlotAddr,
        model: DeviceModel,
        settings: &impl serde::Serialize,
    ) {
        let Some((bus, slot)) = self.slot(addr) else {
            return;
        };
        match DeviceConfig::of_model(&bus.id, &slot.id, model, settings) {
            Ok(config) => self.add_device(config),
            Err(e) => error!("{}: {:#}", self.slot_label(addr), e),
        }
    }

    /// Shares a folder, through the device model this slot's bus has for it
    pub fn attach_file_sharing(&self, addr: SlotAddr, shared_dir: PathBuf) {
        if let Some(model) = self.offered(addr, FunctionKind::FileSharing) {
            self.attach_model(addr, model, &FileSharingSettings { shared_dir });
        }
    }

    /// A reference to the bus list, which the caller can keep while it borrows the rest
    pub fn bus_list(&self) -> Arc<Vec<Bus>> {
        Arc::clone(&self.buses)
    }

    /// Attaches a hard disk, of the model this slot's bus has
    pub fn attach_hard_disk(&self, addr: SlotAddr, media: &Path) {
        if let Some(model) = self.offered(addr, FunctionKind::HardDisk) {
            self.attach_model(
                addr,
                model,
                &HardDiskSettings {
                    media: media.to_path_buf(),
                    read_only: false,
                },
            );
        }
    }

    /// Attaches a hard disk in the first free slot
    pub fn attach_hard_disk_firstfree(&self, media: &Path) -> bool {
        let Some(addr) = self.free_slot(FunctionKind::HardDisk) else {
            return false;
        };
        self.attach_hard_disk(addr, media);
        true
    }

    /// Branches off a disk image to the specified path
    pub fn branch_disk(&self, addr: SlotAddr, path: &Path) {
        if let Some(sender) = self.cmdsender.as_ref() {
            sender
                .send(EmulatorCommand::BranchDisk(addr, path.to_path_buf()))
                .unwrap();
        }
    }

    /// Attaches a CD-ROM drive with this image or folder of images, if any
    pub fn attach_cdrom(&self, addr: SlotAddr, media: Option<PathBuf>) {
        if let Some(model) = self.offered(addr, FunctionKind::Cdrom) {
            self.attach_model(addr, model, &CdromSettings { media });
        }
    }

    /// Replaces the device in a slot by a CD-ROM drive with a folder of images
    pub fn attach_cdrom_folder(&self, addr: SlotAddr, folder: PathBuf) {
        if self.slot(addr).is_some_and(|(_, s)| s.device.is_some()) {
            self.remove_device_quietly(addr);
        }
        self.attach_cdrom(addr, Some(folder));
    }

    /// Puts an image in a CD-ROM drive, attaching one if the slot is empty
    pub fn load_cdrom(&self, addr: SlotAddr, path: &Path) {
        if self.slot(addr).is_some_and(|(_, s)| s.device.is_none()) {
            self.attach_cdrom(addr, Some(path.to_path_buf()));
        } else {
            self.load_media(addr, path, false);
        }
    }

    /// Puts an image in the first empty CD-ROM drive
    pub fn load_cdrom_firstfree(&self, path: &Path) -> bool {
        let free = self.all_slots().find(|(addr, _, _)| {
            matches!(self.status(*addr), Some(FunctionStatus::Cdrom(c)) if c.image.is_none())
        });
        let Some((addr, _, _)) = free else {
            return false;
        };
        self.load_media(addr, path, false);
        true
    }

    /// Puts an image in a drive
    pub fn load_media(&self, addr: SlotAddr, path: &Path, read_only: bool) {
        if let Some(sender) = self.cmdsender.as_ref() {
            sender
                .send(EmulatorCommand::LoadMedia(
                    addr,
                    path.to_path_buf(),
                    read_only,
                ))
                .unwrap();
        }
    }

    /// Takes the image out of a drive
    pub fn eject_media(&self, addr: SlotAddr) {
        if let Some(sender) = self.cmdsender.as_ref() {
            sender.send(EmulatorCommand::EjectMedia(addr)).unwrap();
        }
    }

    fn remove_device_quietly(&self, addr: SlotAddr) {
        if let Some(sender) = self.cmdsender.as_ref() {
            sender.send(EmulatorCommand::RemoveDevice(addr)).unwrap();
        }
    }

    /// Takes a device out of its slot
    pub fn remove_device(&mut self, addr: SlotAddr) {
        self.remove_device_quietly(addr);
        self.messages.push_back((
            UserMessageType::Notice,
            format!(
                "{} detached. System should be restarted.",
                self.slot_label(addr)
            ),
        ));
    }

    /// The folder shared with the guest
    pub fn shared_dir(&self) -> Option<PathBuf> {
        self.all_slots()
            .find_map(|(addr, _, _)| match self.status(addr) {
                Some(FunctionStatus::FileSharing(s)) => Some(s.shared_dir.clone()),
                _ => None,
            })
    }

    /// Shares a folder with the guest, or no more. It takes a slot on the SCSI bus.
    pub fn set_shared_dir(&self, path: Option<PathBuf>) {
        let Some(sender) = self.cmdsender.as_ref() else {
            return;
        };
        let buses = self.buses();
        let Some(bus_idx) = buses.iter().position(|b| b.kind == BusKind::Scsi) else {
            return;
        };
        let slots = &buses[bus_idx].slots;
        let current = slots.iter().position(|s| {
            s.device
                .as_ref()
                .is_some_and(|d| d.model.function() == FunctionKind::FileSharing)
        });
        if let Some(slot) = current {
            sender
                .send(EmulatorCommand::RemoveDevice(SlotAddr {
                    bus: bus_idx,
                    slot,
                }))
                .unwrap();
        }
        let Some(shared_dir) = path else {
            return;
        };
        let free = current.or_else(|| slots.iter().rposition(|s| s.device.is_none()));
        let Some(free) = free else {
            warn!("No free SCSI ID for the shared folder");
            return;
        };
        let Some(model) = self.get_model().and_then(|mac| {
            models_for(BusKind::Scsi, mac).find(|i| i.function == FunctionKind::FileSharing)
        }) else {
            return;
        };
        let config = DeviceConfig::of_model(
            &buses[bus_idx].id,
            &slots[free].id,
            model.model,
            &FileSharingSettings { shared_dir },
        )
        .unwrap();
        sender.send(EmulatorCommand::AddDevice(config)).unwrap();
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

    /// Loads a floppy image from the specified path
    pub fn load_floppy(&self, addr: SlotAddr, path: &Path, wp: bool) {
        self.load_media(addr, path, wp);
    }

    /// Sets the joystick and buttons on the Apple IIe card's game port.
    pub fn set_iie_game_port(&self, port: GamePort) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender.send(EmulatorCommand::SetIieGamePort(port)).unwrap();
    }

    /// Loads the toolbox floppy in the first free drive
    pub fn load_toolbox_floppy(&self) -> bool {
        let Some(ref sender) = self.cmdsender else {
            return false;
        };

        // Find a free floppy drive
        let Some(addr) = self.free_floppy_drive() else {
            return false;
        };
        sender
            .send(EmulatorCommand::InsertFloppyImage(
                addr,
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
        true
    }

    /// Reloads last ejected floppy
    pub fn reload_floppy(&self, addr: SlotAddr) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        let Some(image) = self.last_images.borrow_mut().remove(&addr) else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppyImage(addr, image, false))
            .unwrap();
    }

    /// Inserts a blank floppy
    pub fn insert_blank_floppy(&self, addr: SlotAddr, t: FloppyType) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::InsertFloppyImage(
                addr,
                Box::new(FloppyImage::new(t, "Blank")),
                false,
            ))
            .unwrap();
    }

    /// Saves a floppy image to the specified path.
    pub fn save_floppy(&self, addr: SlotAddr, path: &Path) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::SaveFloppy(addr, path.to_path_buf()))
            .unwrap();
    }

    /// Toggles automatic writeback of a floppy drive's image to its source file.
    pub fn set_floppy_writeback(&self, addr: SlotAddr, enabled: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::SetFloppyWriteback(addr, enabled))
            .unwrap();
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

    pub fn set_debug_framebuffers(&mut self, v: bool) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::SetDebugFramebuffers(v))
            .unwrap();
        self.debug_framebuffers = v;
    }

    pub fn set_floppy_rpm_adjustment(&self, drive: SlotAddr, adjustment: i32) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };
        sender
            .send(EmulatorCommand::SetFloppyRpmAdjustment(drive, adjustment))
            .unwrap();
    }

    /// The slot of a serial port
    fn serial_slot(&self, ch: SccCh) -> Option<SlotAddr> {
        let id = match ch {
            SccCh::A => "A",
            SccCh::B => "B",
        };
        self.all_slots()
            .find(|(_, bus, slot)| bus.kind == BusKind::Serial && slot.id == id)
            .map(|(addr, _, _)| addr)
    }

    /// Plugs a host link in a serial port: a byte link, or a LocalTalk adapter
    pub fn enable_serial_bridge(&self, ch: SccCh, config: &SerialBridgeConfig) -> Result<()> {
        let Some(addr) = self.serial_slot(ch) else {
            return Ok(());
        };
        let (function, settings) = match *config {
            SerialBridgeConfig::Pty => (FunctionKind::SerialLink, SerialLinkSettings::default()),
            SerialBridgeConfig::Tcp(port) => (
                FunctionKind::SerialLink,
                SerialLinkSettings {
                    tcp_port: Some(port),
                },
            ),
            SerialBridgeConfig::LocalTalk => {
                (FunctionKind::LocalTalk, SerialLinkSettings::default())
            }
        };
        if let Some(model) = self.offered(addr, function) {
            self.attach_model(addr, model, &settings);
        }
        Ok(())
    }

    /// Unplugs it
    pub fn disable_serial_bridge(&self, ch: SccCh) -> Result<()> {
        if let Some(addr) = self.serial_slot(ch) {
            self.remove_device_quietly(addr);
        }
        Ok(())
    }

    pub fn get_serial_bridge_status(&self, ch: SccCh) -> Option<&SerialBridgeStatus> {
        match self.status(self.serial_slot(ch)?)? {
            FunctionStatus::SerialLink(s) | FunctionStatus::LocalTalk(s) => s.as_ref(),
            _ => None,
        }
    }

    pub fn is_serial_bridge_enabled(&self, ch: SccCh) -> bool {
        self.serial_slot(ch)
            .and_then(|addr| self.slot(addr))
            .is_some_and(|(_, slot)| slot.device.is_some())
    }

    #[cfg(feature = "ethernet")]
    pub fn set_eth_link(
        &self,
        id: SlotAddr,
        link: snow_core::mac::scsi::ethernet::EthernetLinkType,
    ) {
        let Some(ref sender) = self.cmdsender else {
            return;
        };

        sender
            .send(EmulatorCommand::EthernetSetLink(id, link))
            .unwrap();
    }

    #[cfg(feature = "ethernet")]
    pub fn start_ethernet_capture<P: AsRef<std::path::Path>>(
        &self,
        scsi_id: SlotAddr,
        filename: P,
    ) {
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
    pub fn stop_ethernet_capture(&self, scsi_id: SlotAddr) {
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

/// The position of an HD20 slot on the chain, from 1
fn hd20_position(slot: &Slot) -> Option<usize> {
    slot.id.strip_prefix(HD20_SLOT)?.parse().ok()
}
