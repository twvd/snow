pub mod comm;

#[cfg(feature = "savestates")]
pub mod save;

use serde::{Deserialize, Serialize};
use snow_floppy::loaders::{Autodetect, FloppyImageLoader, FloppyImageSaver, Moof};
use snow_floppy::Floppy;
use std::collections::VecDeque;
#[cfg(feature = "savestates")]
use std::fs::File;
#[cfg(feature = "savestates")]
use std::io::Seek;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, InspectableBus};
use crate::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use crate::cpu_m68k::{CpuM68000, CpuM68020Fpu, CpuM68020Pmmu, CpuM68030Fpu};
use crate::debuggable::{Debuggable, DebuggableProperties};
#[cfg(feature = "savestates")]
use crate::emulator::save::{load_state_from, save_state_to};
use crate::keymap::KeyEvent;
use crate::mac::compact::bus::{CompactMacBus, RAM_DIRTY_PAGESIZE};
use crate::mac::macii::bus::MacIIBus;
use crate::mac::scc::Scc;
use crate::mac::scsi::target::ScsiTargetEvent;
use crate::mac::serial_bridge::{SccBridge, SerialBridgeStatus};
use crate::mac::swim::drive::DriveType;
use crate::mac::{ExtraROMs, MacModel, MacMonitor};
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::AudioReceiver;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

use anyhow::{bail, Context, Result};
use bit_set::BitSet;
use log::*;
use std::fmt;

use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::emulator::comm::{EmulatorSpeed, UserMessageType};
use crate::mac::rtc::Rtc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use comm::{
    Breakpoint, EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver,
    EmulatorStatus, FddStatus, InputRecording, ScsiTargetStatus,
};

/// Mouse emulation mode
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, strum::EnumIter)]
pub enum MouseMode {
    /// Absolute with memory hack (original software only)
    #[default]
    Absolute,
    /// Relative through hardware emulation
    RelativeHw,
    /// Disabled
    Disabled,
}

impl fmt::Display for MouseMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absolute => write!(f, "Absolute (memory patching)"),
            Self::RelativeHw => write!(f, "Relative (hardware emulation)"),
            Self::Disabled => write!(f, "Disabled"),
        }
    }
}

macro_rules! dispatch {
    (
        // Immutable references (&self -> &Type)
        immutable_refs {
            $( fn $ref_method:ident(&self) -> $ref_ret:ty { $($ref_target:tt)* } )*
        }

        // Mutable references (&mut self -> &mut Type)
        mutable_refs {
            $( fn $mut_ref_method:ident(&mut self) -> $mut_ref_ret:ty { $($mut_ref_target:tt)* } )*
        }

        // Immutable method calls (&self, args... -> RetType)
        immutable_calls {
            $( fn $immut_call_method:ident(&self $(, $immut_arg:ident: $immut_arg_ty:ty)*) -> $immut_call_ret:ty { $($immut_call_target:tt)* } )*
        }

        // Mutable method calls (&mut self, args... -> RetType)
        mutable_calls {
            $( fn $mut_call_method:ident(&mut self $(, $mut_arg:ident: $mut_arg_ty:ty)*) -> $mut_call_ret:ty { $($mut_call_target:tt)* } )*
        }
    ) => {
        #[allow(dead_code)]
        impl EmulatorConfig {
            // Generate immutable reference methods
            $(
                pub fn $ref_method(&self) -> $ref_ret {
                    match self {
                        Self::Compact(inner) => &inner.$($ref_target)*,
                        Self::MacII(inner) => &inner.$($ref_target)*,
                        Self::MacIIPmmu(inner) => &inner.$($ref_target)*,
                        Self::MacII30(inner) => &inner.$($ref_target)*,
                    }
                }
            )*

            // Generate mutable reference methods
            $(
                pub fn $mut_ref_method(&mut self) -> $mut_ref_ret {
                    match self {
                        Self::Compact(inner) => &mut inner.$($mut_ref_target)*,
                        Self::MacII(inner) => &mut inner.$($mut_ref_target)*,
                        Self::MacIIPmmu(inner) => &mut inner.$($mut_ref_target)*,
                        Self::MacII30(inner) => &mut inner.$($mut_ref_target)*,
                    }
                }
            )*

            // Generate immutable method calls
            $(
                pub fn $immut_call_method(&self $(, $immut_arg: $immut_arg_ty)*) -> $immut_call_ret {
                    match self {
                        Self::Compact(inner) => inner.$($immut_call_target)*,
                        Self::MacII(inner) => inner.$($immut_call_target)*,
                        Self::MacIIPmmu(inner) => inner.$($immut_call_target)*,
                        Self::MacII30(inner) => inner.$($immut_call_target)*,
                    }
                }
            )*

            // Generate mutable method calls
            $(
                pub fn $mut_call_method(&mut self $(, $mut_arg: $mut_arg_ty)*) -> $mut_call_ret {
                    match self {
                        Self::Compact(inner) => inner.$($mut_call_target)*,
                        Self::MacII(inner) => inner.$($mut_call_target)*,
                        Self::MacIIPmmu(inner) => inner.$($mut_call_target)*,
                        Self::MacII30(inner) => inner.$($mut_call_target)*,
                    }
                }
            )*
        }
    };
}

/// Emulator config. Basically an abstraction on top of the CPU for multiple different model groups
/// that provides access to the inner components by the emulator runner through dynamic dispatch.
#[derive(Serialize, Deserialize)]
enum EmulatorConfig {
    /// Compact series - Mac 128K, 512K, Plus, SE, Classic
    Compact(Box<CpuM68000<CompactMacBus<ChannelRenderer>>>),
    /// Macintosh II (AMU)
    MacII(Box<CpuM68020Fpu<MacIIBus<ChannelRenderer, true>>>),
    /// Macintosh II (PMMU)
    MacIIPmmu(Box<CpuM68020Pmmu<MacIIBus<ChannelRenderer, false>>>),
    /// Macintosh SE/30 and 68030-based Macintosh IIs
    MacII30(Box<CpuM68030Fpu<MacIIBus<ChannelRenderer, false>>>),
}

dispatch! {
    immutable_refs {
        fn swim(&self) -> &Swim { bus.swim }
        fn scsi(&self) -> &ScsiController { bus.scsi }
        fn scc(&self) -> &Scc { bus.scc }
        fn cpu_regs(&self) -> &RegisterFile { regs }
        fn ram(&self) -> &[u8] { bus.ram }
        fn ram_dirty(&self) -> &BitSet { bus.ram_dirty }
    }

    mutable_refs {
        fn swim_mut(&mut self) -> &mut Swim { bus.swim }
        fn scsi_mut(&mut self) -> &mut ScsiController { bus.scsi }
        fn scc_mut(&mut self) -> &mut Scc { bus.scc }
        fn cpu_regs_mut(&mut self) -> &mut RegisterFile { regs }
        fn ram_mut(&mut self) -> &mut [u8] { bus.ram }
        fn ram_dirty_mut(&mut self) -> &mut BitSet { bus.ram_dirty }
    }

    immutable_calls {
        fn model(&self) -> MacModel { bus.model() }
        fn cpu_cycles(&self) -> Ticks { cycles }
        fn cpu_breakpoints(&self) -> &[Breakpoint] { breakpoints() }
        fn cpu_get_step_over(&self) -> Option<Address> { get_step_over() }
        fn speed(&self) -> EmulatorSpeed { bus.speed }
        fn debug_properties(&self) -> DebuggableProperties { bus.get_debug_properties() }
        fn get_audio_channel(&self) -> AudioReceiver { bus.get_audio_channel() }
    }

    mutable_calls {
        fn set_speed(&mut self, speed: EmulatorSpeed) -> () { bus.set_speed(speed) }

        fn cpu_tick(&mut self, ticks: Ticks) -> Result<Ticks> { tick(ticks) }
        fn cpu_set_breakpoint(&mut self, bp: Breakpoint) -> () { set_breakpoint(bp) }
        fn cpu_breakpoints_mut(&mut self) -> &mut Vec<Breakpoint> { breakpoints_mut() }
        fn cpu_clear_breakpoint(&mut self, bp: Breakpoint) -> () { clear_breakpoint(bp) }
        fn cpu_enable_history(&mut self, v: bool) -> () { enable_history(v) }
        fn cpu_enable_systrap_history(&mut self, v: bool) -> () { enable_systrap_history(v) }
        fn cpu_set_pc(&mut self, pc: Address) -> Result<()> { set_pc(pc) }
        fn cpu_get_clr_breakpoint_hit(&mut self) -> bool { get_clr_breakpoint_hit() }
        fn cpu_read_history(&mut self) -> Option<&[HistoryEntry]> { read_history() }
        fn cpu_read_systrap_history(&mut self) -> Option<&[SystrapHistoryEntry]> { read_systrap_history() }
        fn cpu_prefetch_refill(&mut self) -> Result<()> { prefetch_refill() }
        fn cpu_reset(&mut self) -> Result<()> { reset() }

        fn bus_reset(&mut self) -> Result<()> { bus.reset(true) }
        fn after_deserialize(&mut self, renderer: ChannelRenderer) -> () { bus.after_deserialize(renderer) }
        fn bus_write(&mut self, addr: Address, val: Byte) -> crate::bus::BusResult<Byte> { bus.write(addr, val) }
        fn bus_inspect_read(&mut self, addr: Address) -> Option<Byte> { bus.inspect_read(addr) }
        fn bus_inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> { bus.inspect_write(addr, val) }

        fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) -> () { bus.mouse_update_rel(relx, rely, button) }
        fn mouse_update_abs(&mut self, x: u16, y: u16) -> () { bus.mouse_update_abs(x, y) }
        fn keyboard_event(&mut self, ke: KeyEvent) -> () { bus.keyboard_event(ke) }
        fn input_release_all(&mut self) -> () { bus.input_release_all() }
        fn progkey(&mut self) -> () { bus.progkey() }
        fn video_blank(&mut self) -> Result<()> { bus.video_blank() }

        fn rtc_mut(&mut self) -> &mut Rtc { bus.rtc_mut() }
    }
}

/// Emulator runner
pub struct Emulator {
    config: EmulatorConfig,
    command_recv: crossbeam_channel::Receiver<EmulatorCommand>,
    command_sender: EmulatorCommandSender,
    event_sender: crossbeam_channel::Sender<EmulatorEvent>,
    event_recv: EmulatorEventReceiver,
    run: bool,
    last_update: Instant,
    model: MacModel,
    record_input: Option<InputRecording>,
    replay_input: VecDeque<(Ticks, EmulatorCommand)>,
    peripheral_debug: bool,
    /// Serial bridges for SCC channels (index 0 = Channel A, index 1 = Channel B)
    serial_bridges: [Option<SccBridge>; 2],
}

impl Emulator {
    pub fn new(
        rom: &[u8],
        extra_roms: &[ExtraROMs],
        model: MacModel,
    ) -> Result<(Self, crossbeam_channel::Receiver<DisplayBuffer>)> {
        Self::new_with_extra(
            rom,
            extra_roms,
            model,
            None,
            MouseMode::default(),
            None,
            None,
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_extra(
        rom: &[u8],
        extra_roms: &[ExtraROMs],
        model: MacModel,
        monitor: Option<MacMonitor>,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
        override_fdd_type: Option<DriveType>,
        pmmu_enabled: bool,
        shared_dir: Option<PathBuf>,
    ) -> Result<(Self, crossbeam_channel::Receiver<DisplayBuffer>)> {
        // Set up channels
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();
        let renderer = ChannelRenderer::new(0, 0)?;
        let frame_recv = renderer.get_receiver();

        let mut config = match model {
            MacModel::Early128K
            | MacModel::Early512K
            | MacModel::Early512Ke
            | MacModel::Plus
            | MacModel::SE
            | MacModel::SeFdhd
            | MacModel::Classic => {
                assert!(!pmmu_enabled, "PMMU not available on compact models");

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = CompactMacBus::new(
                    model,
                    rom,
                    extension_rom,
                    renderer,
                    mouse_mode,
                    ram_size,
                    override_fdd_type,
                );
                let cpu = Box::new(CpuM68000::new(bus));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::Compact(cpu)
            }
            MacModel::MacII | MacModel::MacIIFDHD => {
                assert!(override_fdd_type.is_none());

                // Find display card ROM
                let Some(ExtraROMs::MDC12(mdcrom)) =
                    extra_roms.iter().find(|p| matches!(p, ExtraROMs::MDC12(_)))
                else {
                    bail!("Macintosh II requires display card ROM")
                };

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                if !pmmu_enabled {
                    // Initialize bus and CPU
                    let bus = MacIIBus::new(
                        model,
                        rom,
                        mdcrom,
                        extension_rom,
                        vec![renderer],
                        monitor.unwrap_or_default(),
                        mouse_mode,
                        ram_size,
                    );
                    let cpu = Box::new(CpuM68020Fpu::new(bus));
                    assert_eq!(cpu.get_type(), model.cpu_type());

                    EmulatorConfig::MacII(cpu)
                } else {
                    // Initialize bus and CPU
                    let bus = MacIIBus::new(
                        model,
                        rom,
                        mdcrom,
                        extension_rom,
                        vec![renderer],
                        monitor.unwrap_or_default(),
                        mouse_mode,
                        ram_size,
                    );
                    let cpu = Box::new(CpuM68020Pmmu::new(bus));
                    assert_eq!(cpu.get_type(), model.cpu_type());

                    EmulatorConfig::MacIIPmmu(cpu)
                }
            }
            MacModel::MacIIx | MacModel::MacIIcx => {
                assert!(override_fdd_type.is_none());

                // Find display card ROM
                let Some(ExtraROMs::MDC12(mdcrom)) =
                    extra_roms.iter().find(|p| matches!(p, ExtraROMs::MDC12(_)))
                else {
                    bail!("Macintosh II requires display card ROM")
                };

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = MacIIBus::new(
                    model,
                    rom,
                    mdcrom,
                    extension_rom,
                    vec![renderer],
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                );
                let cpu = Box::new(CpuM68030Fpu::new(bus));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::MacII30(cpu)
            }
            MacModel::SE30 => {
                assert!(override_fdd_type.is_none());

                // Find video ROM
                let Some(ExtraROMs::SE30Video(vrom)) = extra_roms
                    .iter()
                    .find(|p| matches!(p, ExtraROMs::SE30Video(_)))
                else {
                    bail!("Macintosh SE/30 requires video ROM")
                };

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = MacIIBus::new(
                    model,
                    rom,
                    vrom,
                    extension_rom,
                    vec![renderer],
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                );
                let cpu = Box::new(CpuM68030Fpu::new(bus));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::MacII30(cpu)
            }
        };

        config.scsi_mut().set_shared_dir(shared_dir);
        config.cpu_reset()?;

        let mut emu = Self {
            config,
            command_recv: cmdr,
            command_sender: cmds,
            event_sender: statuss,
            event_recv: statusr,
            run: false,
            last_update: Instant::now(),
            model,
            record_input: None,
            replay_input: VecDeque::default(),
            peripheral_debug: false,
            serial_bridges: [None, None],
        };
        emu.status_update()?;

        for ch in crate::mac::scc::SccCh::iter() {
            emu.event_sender
                .send(EmulatorEvent::SerialBridgeStatus(ch, None))?;
        }

        Ok((emu, frame_recv))
    }

    /// Restores a saved emulator state into a new Emulator instance
    #[cfg(feature = "savestates")]
    pub fn load_state<P: AsRef<Path>, PT: AsRef<Path>>(
        path: P,
        tmpdir: PT,
    ) -> Result<(Self, crossbeam_channel::Receiver<DisplayBuffer>)> {
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();
        let renderer = ChannelRenderer::new(0, 0)?;
        let frame_recv = renderer.get_receiver();
        let time = Instant::now();

        let fstr = path
            .as_ref()
            .file_name()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let f = File::open(path)?;

        let mut config = load_state_from(f, tmpdir)?;
        config.after_deserialize(renderer);

        let model = config.model();

        let mut emu = Self {
            config,
            command_recv: cmdr,
            command_sender: cmds,
            event_sender: statuss,
            event_recv: statusr,
            run: false,
            last_update: Instant::now(),
            model,
            record_input: None,
            replay_input: VecDeque::default(),
            peripheral_debug: false,
            serial_bridges: [None, None],
        };
        emu.status_update()?;

        for ch in crate::mac::scc::SccCh::iter() {
            emu.event_sender
                .send(EmulatorEvent::SerialBridgeStatus(ch, None))?;
        }

        log::info!(
            "Restored save state {} ({}) in {:?}",
            fstr,
            model,
            time.elapsed()
        );

        Ok((emu, frame_recv))
    }

    /// Sets a path to persist the PRAM in. If the file exists, it is loaded. Otherwise, an empty
    /// file is created. The PRAM file is continuously updated.
    pub fn persist_pram(&mut self, pram_path: &Path) {
        self.config.rtc_mut().load_pram(pram_path);
    }

    /// Sets the RTC to a specific date/time.
    /// This can be used to test date-dependent software behavior (e.g., easter eggs).
    pub fn set_datetime(&mut self, dt: chrono::NaiveDateTime) {
        self.config.rtc_mut().set_datetime(dt);
    }

    pub fn create_cmd_sender(&self) -> EmulatorCommandSender {
        self.command_sender.clone()
    }

    pub fn create_event_recv(&self) -> EmulatorEventReceiver {
        self.event_recv.clone()
    }

    fn status_update(&mut self) -> Result<()> {
        for (i, drive) in self.config.swim_mut().drives.iter_mut().enumerate() {
            if let Some(img) = drive.take_ejected_image() {
                self.event_sender
                    .send(EmulatorEvent::FloppyEjected(i, img))?;
            }
        }
        for (id, target) in self
            .config
            .scsi_mut()
            .targets
            .iter_mut()
            .enumerate()
            .filter_map(|(i, t)| t.as_mut().map(|t| (i, t)))
        {
            match target.take_event() {
                Some(ScsiTargetEvent::MediaEjected) => {
                    self.event_sender
                        .send(EmulatorEvent::ScsiMediaEjected(id))
                        .unwrap();
                }
                None => (),
            }
        }

        self.event_sender
            .send(EmulatorEvent::Status(Box::new(EmulatorStatus {
                regs: self.config.cpu_regs().clone(),
                running: self.run,
                breakpoints: self.config.cpu_breakpoints().to_vec(),
                cycles: self.config.cpu_cycles(),
                fdd: core::array::from_fn(|i| FddStatus {
                    present: self.config.swim().drives[i].is_present(),
                    ejected: !self.config.swim().drives[i].floppy_inserted,
                    motor: self.config.swim().drives[i].motor,
                    writing: self.config.swim().drives[i].motor && self.config.swim().is_writing(),
                    track: self.config.swim().drives[i].track,
                    image_title: self.config.swim().drives[i].floppy.get_title().to_owned(),
                    dirty: self.config.swim().drives[i].floppy.is_dirty(),
                    drive_type: self.config.swim().drives[i].drive_type,
                }),
                model: self.model,
                scsi: core::array::from_fn(|i| {
                    self.config
                        .scsi()
                        .get_target_type(i)
                        .map(|t| ScsiTargetStatus {
                            target_type: t,
                            capacity: self.config.scsi().get_disk_capacity(i),
                            image: self
                                .config
                                .scsi()
                                .get_disk_imagefn(i)
                                .map(|p| p.to_path_buf()),
                            #[cfg(feature = "ethernet")]
                            link_type: self.config.scsi().targets[i]
                                .as_ref()
                                .and_then(|d| d.eth_link()),
                        })
                }),
                speed: self.config.speed(),
            })))?;

        // Next code stream for disassembly listing
        self.disassemble(self.config.cpu_regs().pc, 200)?;

        // Memory contents
        for page in self.config.ram_dirty() {
            let r = (page * RAM_DIRTY_PAGESIZE)..((page + 1) * RAM_DIRTY_PAGESIZE);
            self.event_sender.send(EmulatorEvent::Memory((
                r.start as Address,
                self.config.ram()[r].to_vec(),
                self.config.ram().len(),
            )))?;
        }
        self.config.ram_dirty_mut().clear();

        // Instruction history
        if let Some(history) = self.config.cpu_read_history() {
            self.event_sender
                .send(EmulatorEvent::InstructionHistory(history.to_vec()))?;
        }

        // System trap history
        if let Some(history) = self.config.cpu_read_systrap_history() {
            self.event_sender
                .send(EmulatorEvent::SystrapHistory(history.to_vec()))?;
        }

        // Peripheral debug view
        if self.peripheral_debug {
            self.event_sender.send(EmulatorEvent::PeripheralDebug(
                self.config.debug_properties(),
            ))?;
        }

        Ok(())
    }

    fn disassemble(&mut self, addr: Address, len: usize) -> Result<()> {
        let ops = (addr..)
            .take(len)
            .flat_map(|addr| self.config.bus_inspect_read(addr))
            .collect::<Vec<_>>();

        self.event_sender
            .send(EmulatorEvent::NextCode((addr, ops)))?;

        Ok(())
    }

    /// Steps the emulator by one instruction.
    fn step(&mut self) -> Result<()> {
        let mut stop_break = false;
        self.config.cpu_tick(1)?;

        // Mac 512K: 0x402154, Mac Plus: 0x418CCC
        //if self.config.swim().drives[0].track == 2 {
        //    if self.config.cpu_regs().pc == 0x418CCC {
        //        debug!(
        //            "Sony_RdAddr = {}, format: {:02X}, track: {}, sector: {}",
        //            self.config.cpu_regs().d[0] as i32,
        //            self.config.cpu_regs().d[3] as u8,
        //            self.config.cpu_regs().d[1] as u16,
        //            self.config.cpu_regs().d[2] as u16,
        //        );
        //    }
        //    if self.config.cpu_regs().pc == 0x418EBC {
        //        debug!("Sony_RdData = {}", self.config.cpu_regs().d[0] as i32);
        //    }
        //}

        if self.run && self.config.cpu_get_clr_breakpoint_hit() {
            stop_break = true;
        }
        if stop_break {
            self.run = false;
            self.status_update()?;
        }
        Ok(())
    }

    pub fn get_audio(&mut self) -> AudioReceiver {
        self.config.get_audio_channel()
    }

    pub fn load_hdd_image(&mut self, filename: &Path, scsi_id: usize) -> Result<()> {
        self.config.scsi_mut().attach_hdd_at(filename, scsi_id)
    }

    fn user_error(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Error,
                msg.to_owned(),
            ))
            .unwrap();
        error!("{}", msg);
    }

    #[allow(dead_code)]
    fn user_warning(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Warning,
                msg.to_owned(),
            ))
            .unwrap();
        warn!("{}", msg);
    }

    #[allow(dead_code)]
    fn user_notice(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Notice,
                msg.to_owned(),
            ))
            .unwrap();
        info!("{}", msg);
    }

    fn user_success(&self, msg: &str) {
        self.event_sender
            .send(EmulatorEvent::UserMessage(
                UserMessageType::Success,
                msg.to_owned(),
            ))
            .unwrap();
        info!("{}", msg);
    }

    #[inline(always)]
    fn try_step(&mut self) {
        if let Err(e) = self.step() {
            self.run = false;
            self.user_error(&format!(
                "Emulator halted: Uncaught CPU stepping error at PC {:08X}: {:?}",
                self.config.cpu_regs().pc,
                e
            ));
            let _ = self.status_update();
        }
    }

    pub fn get_cycles(&self) -> Ticks {
        self.config.cpu_cycles()
    }

    pub fn attach_cdrom(&mut self, id: usize) {
        self.config.scsi_mut().attach_cdrom_at(id);
        info!("SCSI ID #{}: CD-ROM drive attached", id);
    }

    #[cfg(feature = "ethernet")]
    pub fn attach_ethernet(&mut self, id: usize) {
        self.config.scsi_mut().attach_ethernet_at(id);
        info!("SCSI ID #{}: Ethernet controller attached", id);
    }

    #[cfg(feature = "savestates")]
    fn save_state(&self, p: &Path, screenshot: Option<Vec<u8>>) -> Result<()> {
        let mut f = File::create(p)?;
        let time = Instant::now();

        save_state_to(&f, &self.config, screenshot)?;

        log::info!(
            "Wrote state to {} in {:?} ({} bytes)",
            p.file_name()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            time.elapsed(),
            f.stream_position()?
        );
        Ok(())
    }
}

impl Tickable for Emulator {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        if !self.command_recv.is_empty() {
            while let Ok(cmd) = self.command_recv.try_recv() {
                let cycles = self.get_cycles();

                match cmd {
                    EmulatorCommand::MouseUpdateRelative { relx, rely, btn } => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        self.config.mouse_update_rel(relx, rely, btn);
                    }
                    EmulatorCommand::MouseUpdateAbsolute { x, y } => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        self.config.mouse_update_abs(x, y);
                    }
                    EmulatorCommand::Quit => {
                        info!("Emulator terminating");
                        self.config.video_blank()?;
                        return Ok(0);
                    }
                    EmulatorCommand::InsertFloppy(drive, filename, wp) => {
                        let image = Autodetect::load_file(&filename);
                        match image {
                            Ok(mut img) => {
                                if wp {
                                    img.set_force_wp();
                                }
                                if let Err(e) = self.config.swim_mut().disk_insert(drive, img) {
                                    self.user_error(&format!("Cannot insert disk: {}", e));
                                }
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "Cannot load image '{}': {:?}",
                                    filename, e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::InsertFloppyImage(drive, mut img, wp) => {
                        if wp {
                            img.set_force_wp();
                        }
                        if let Err(e) = self.config.swim_mut().disk_insert(drive, *img) {
                            self.user_error(&format!("Cannot insert disk: {}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EjectFloppy(drive) => {
                        self.config.swim_mut().drives[drive].eject();
                    }
                    EmulatorCommand::ScsiAttachHdd(id, filename) => {
                        match self.load_hdd_image(&filename, id) {
                            Ok(_) => {
                                info!(
                                    "SCSI ID #{}: hard drive attached, image '{}' loaded",
                                    id,
                                    filename.display()
                                );
                            }
                            Err(e) => {
                                self.user_error(&format!("SCSI ID #{}: {:#}", id, e));
                            }
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::ScsiBranchHdd(id, filename) => {
                        match self.config.scsi_mut().targets[id]
                            .as_mut()
                            .context("No target attached")?
                            .branch_media(&filename)
                        {
                            Ok(_) => {
                                info!("SCSI ID #{}: branched to file '{}'", id, filename.display());
                            }
                            Err(e) => {
                                self.user_error(&format!("SCSI ID #{}: {:#}", id, e));
                            }
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::ScsiLoadMedia(id, filename) => {
                        match self.config.scsi_mut().targets[id]
                            .as_mut()
                            .context("No target attached")?
                            .load_media(&filename)
                        {
                            Ok(_) => {
                                info!("SCSI ID #{}: image '{}' loaded", id, filename.display());
                            }
                            Err(e) => {
                                self.user_error(&format!("SCSI ID #{}: {:#}", id, e));
                            }
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::ScsiAttachCdrom(id) => {
                        self.attach_cdrom(id);
                        self.status_update()?;
                    }
                    #[cfg(feature = "ethernet")]
                    EmulatorCommand::ScsiAttachEthernet(id) => {
                        self.attach_ethernet(id);
                        self.status_update()?;
                    }
                    EmulatorCommand::DetachScsiTarget(id) => {
                        self.config.scsi_mut().detach_target(id);
                        info!("SCSI ID #{}: target detached", id);
                        self.status_update()?;
                    }
                    EmulatorCommand::SaveFloppy(drive, filename) => {
                        if let Err(e) = Moof::save_file(
                            self.config.swim().get_active_image(drive),
                            &filename.to_string_lossy(),
                        ) {
                            self.user_error(&format!(
                                "Cannot save file '{}': {}",
                                filename.file_name().unwrap_or_default().to_string_lossy(),
                                e
                            ));
                        } else {
                            self.user_success(&format!(
                                "Saved floppy image as '{}'",
                                filename.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::Run => {
                        info!("Running");
                        self.run = true;
                        self.config.cpu_get_clr_breakpoint_hit();
                        self.config.cpu_breakpoints_mut().retain(|bp| {
                            !matches!(bp, Breakpoint::StepOver(_) | Breakpoint::StepOut(_))
                        });
                        self.status_update()?;
                    }
                    EmulatorCommand::Reset => {
                        // Reset bus first so VIA comes back into overlay mode before resetting the CPU
                        // otherwise the wrong reset vector is loaded.
                        self.config.bus_reset()?;
                        self.config.cpu_reset()?;
                        self.config.video_blank()?;

                        info!("Emulator reset");
                        self.status_update()?;
                    }
                    EmulatorCommand::Stop => {
                        info!("Stopped");
                        self.run = false;
                        self.config.cpu_breakpoints_mut().retain(|bp| {
                            !matches!(bp, Breakpoint::StepOver(_) | Breakpoint::StepOut(_))
                        });
                        self.status_update()?;
                    }
                    EmulatorCommand::Step => {
                        if !self.run {
                            self.try_step();
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::StepOut => {
                        if !self.run {
                            self.config.cpu_set_breakpoint(Breakpoint::StepOut(
                                self.config.cpu_regs().read_a(7),
                            ));
                            self.run = true;
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::StepOver => {
                        if !self.run {
                            self.try_step();
                            if let Some(addr) = self.config.cpu_get_step_over() {
                                self.config.cpu_set_breakpoint(Breakpoint::StepOver(addr));
                                self.run = true;
                            }
                            self.status_update()?;
                        }
                    }
                    EmulatorCommand::ToggleBreakpoint(bp) => {
                        let exists = self.config.cpu_breakpoints().contains(&bp);
                        if exists {
                            self.config.cpu_clear_breakpoint(bp);
                            info!("Breakpoint removed: {:X?}", bp);
                        } else {
                            self.config.cpu_set_breakpoint(bp);
                            info!("Breakpoint set: {:X?}", bp);
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::BusInspectWrite(start, data) => {
                        for (i, d) in data.into_iter().enumerate() {
                            let addr = start.wrapping_add(i as Address);
                            if self.config.bus_inspect_write(addr, d).is_none() {
                                self.user_error(&format!(
                                    "Could not write to address {:08X}",
                                    addr
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::Disassemble(addr, len) => {
                        self.disassemble(addr, len)?;
                        // Skip status update which would reset the disassembly view
                        return Ok(ticks);
                    }
                    EmulatorCommand::KeyEvent(e) => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        if !self.run {
                            info!("Ignoring keyboard input while stopped");
                        } else {
                            self.config.keyboard_event(e);
                        }
                    }
                    EmulatorCommand::ReleaseAllInputs => {
                        self.config.input_release_all();
                    }
                    EmulatorCommand::CpuSetPC(val) => self.config.cpu_set_pc(val)?,
                    EmulatorCommand::SetSpeed(s) => self.config.set_speed(s),
                    EmulatorCommand::ProgKey => self.config.progkey(),
                    EmulatorCommand::WriteRegister(reg, val) => {
                        match reg {
                            Register::PC => {
                                if val & 1 != 0 {
                                    self.user_error("Program Counter must be aligned");
                                } else {
                                    self.config.cpu_set_pc(val)?;
                                    self.config.cpu_prefetch_refill()?;
                                }
                            }
                            _ => self.config.cpu_regs_mut().write(reg, val),
                        };
                        self.status_update()?;
                    }
                    EmulatorCommand::StartRecordingInput => {
                        self.record_input = Some(InputRecording::default());
                    }
                    EmulatorCommand::EndRecordingInput => {
                        self.event_sender.send(EmulatorEvent::RecordedInput(
                            self.record_input.take().expect("Recording was not active"),
                        ))?;
                    }
                    EmulatorCommand::ReplayInputRecording(rec, immediately) => {
                        let cycles = self.get_cycles();
                        if rec.is_empty() {
                            break;
                        }

                        // On 'immediately', we skip the delay before the first step and
                        // then continue with the relative cycle delays.
                        //
                        // This is useful if you want to replay a recording once the
                        // system has already been running.
                        let recording_offset = if immediately { rec[0].0 } else { 0 };

                        self.replay_input = VecDeque::from_iter(
                            rec.into_iter()
                                // Offset by current cycles so we can just compare to absolute
                                // cycles later.
                                .map(|(t, c)| (t - recording_offset + cycles, c)),
                        );
                    }
                    EmulatorCommand::SetInstructionHistory(v) => self.config.cpu_enable_history(v),
                    EmulatorCommand::SetSystrapHistory(v) => {
                        self.config.cpu_enable_systrap_history(v);
                    }
                    EmulatorCommand::SetSharedDir(path) => {
                        self.config.scsi_mut().set_shared_dir(path);
                    }
                    EmulatorCommand::SetPeripheralDebug(v) => {
                        self.peripheral_debug = v;
                        self.status_update()?;
                    }
                    EmulatorCommand::SccReceiveData(ch, data) => {
                        self.config.scc_mut().push_rx(ch, &data);
                    }
                    EmulatorCommand::SerialBridgeEnable(ch, config) => {
                        let ch_idx = ch as usize;
                        match SccBridge::new(&config) {
                            Ok(bridge) => {
                                let status = bridge.status();
                                info!("SCC bridge enabled on channel {:?}: {}", ch, status);
                                match &status {
                                    SerialBridgeStatus::Pty(ref path) => {
                                        self.user_notice(&format!(
                                            "Serial bridge PTY: {}",
                                            path.display()
                                        ));
                                    }
                                    SerialBridgeStatus::LocalTalk(_) => {
                                        self.user_notice("LocalTalk bridge enabled");
                                    }
                                    _ => {}
                                }
                                self.serial_bridges[ch_idx] = Some(bridge);
                                self.event_sender
                                    .send(EmulatorEvent::SerialBridgeStatus(ch, Some(status)))?;
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "Failed to enable SCC bridge on channel {:?}: {}",
                                    ch, e
                                ));
                            }
                        }
                    }
                    EmulatorCommand::SerialBridgeDisable(ch) => {
                        let ch_idx = ch as usize;
                        if self.serial_bridges[ch_idx].take().is_some() {
                            info!("Serial bridge disabled on channel {:?}", ch);
                            self.event_sender
                                .send(EmulatorEvent::SerialBridgeStatus(ch, None))?;
                        }
                    }
                    #[cfg(feature = "savestates")]
                    EmulatorCommand::SaveState(path, screenshot) => {
                        if let Err(e) = self.save_state(&path, screenshot) {
                            self.user_error(&format!("Failed to save state: {:?}", e));
                        }
                    }
                    EmulatorCommand::SetDebugFramebuffers(v) => {
                        if let EmulatorConfig::Compact(ref mut c) = &mut self.config {
                            c.bus.video.debug_framebuffers = v;
                        }
                    }
                    EmulatorCommand::SetFloppyRpmAdjustment(drive, adjustment) => {
                        if drive < self.config.swim_mut().drives.len() {
                            self.config.swim_mut().drives[drive].rpm_adjustment = adjustment;
                        }
                    }
                    #[cfg(feature = "ethernet")]
                    EmulatorCommand::EthernetSetLink(idx, link) => {
                        self.config.scsi_mut().targets[idx]
                            .as_mut()
                            .context("Setting link on non-ethernet device")?
                            .eth_set_link(link)?;
                    }
                }
            }
        }

        if self.run {
            if self.last_update.elapsed() > Duration::from_millis(500) {
                self.last_update = Instant::now();
                self.status_update()?;
            }

            // Poll SCC TX data and serial/LocalTalk bridges every tick batch
            // This needs to be frequent for LocalTalk to work properly
            for ch in crate::mac::scc::SccCh::iter() {
                let ch_idx = ch as usize;

                // Check for TX data from SCC
                if self.config.scc().has_tx_data(ch) {
                    let tx_data = self.config.scc_mut().take_tx(ch);

                    // Route through bridge if active, otherwise send to frontend
                    if let Some(ref mut bridge) = self.serial_bridges[ch_idx] {
                        bridge.write_from_scc(&tx_data);
                    } else {
                        self.event_sender
                            .send(EmulatorEvent::SccTransmitData(ch, tx_data))?;
                    }
                }

                // Poll bridges for incoming data and status changes
                if let Some(ref mut bridge) = self.serial_bridges[ch_idx] {
                    // Check for state changes (e.g., new TCP connection, LocalTalk status)
                    let has_data = bridge.poll();
                    if has_data {
                        self.event_sender
                            .send(EmulatorEvent::SerialBridgeStatus(ch, Some(bridge.status())))?;
                    }

                    // Read incoming data from bridge and send to SCC
                    let rx_data = bridge.read_to_scc();
                    if !rx_data.is_empty() {
                        self.config.scc_mut().push_rx(ch, &rx_data);
                    }
                }
            }

            // Replay next step in recording if currently replaying
            if let Some((t, c)) = self.replay_input.front() {
                if *t <= self.get_cycles() {
                    self.command_sender.send(c.clone()).unwrap();
                    self.replay_input.pop_front().unwrap();
                }
            }

            // Batch 10000 steps for performance reasons
            for _ in 0..10000 {
                if !self.run {
                    break;
                }
                self.try_step();

                // Demand-driven LocalTalk polling: poll immediately when Mac re-enables RX
                // Only poll if RX is enabled and no character is available (ready for new packet)
                if self
                    .config
                    .scc_mut()
                    .take_localtalk_poll_needed(crate::mac::scc::SccCh::B)
                    && self
                        .config
                        .scc()
                        .is_rx_ready_for_data(crate::mac::scc::SccCh::B)
                {
                    if let Some(ref mut bridge) = self.serial_bridges[1] {
                        if bridge.is_localtalk() {
                            // First, flush any pending TX data to the bridge
                            // This ensures RTS is processed and CTS is synthesized
                            // before we poll for responses
                            if self.config.scc().has_tx_data(crate::mac::scc::SccCh::B) {
                                let tx_data =
                                    self.config.scc_mut().take_tx(crate::mac::scc::SccCh::B);
                                bridge.write_from_scc(&tx_data);
                            }

                            // Now poll for incoming data and pending CTS
                            bridge.poll();
                            let rx_data = bridge.read_to_scc();
                            if !rx_data.is_empty() {
                                self.config
                                    .scc_mut()
                                    .push_rx(crate::mac::scc::SccCh::B, &rx_data);
                            }
                        }
                    }
                }
            }
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        Ok(ticks)
    }
}
