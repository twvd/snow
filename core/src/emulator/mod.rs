pub mod comm;
mod devices;

#[cfg(feature = "savestates")]
pub mod save;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
#[cfg(feature = "savestates")]
use std::fs::File;
#[cfg(feature = "savestates")]
use std::io::Seek;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, InspectRun, InspectableBus};
use crate::cpu_m68k::bus::InspectView;
use crate::cpu_m68k::cpu::{HistoryEntry, SystrapHistoryEntry};
use crate::cpu_m68k::{
    CpuM68000, CpuM68020, CpuM68030, CpuM68040, FPU_M68040, FPU_M68881, FPU_M68882, FPU_NONE,
};
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::devices::{
    AppleIIeCardFunction, BusOps, DeviceModel, FunctionMut, FunctionStatus, bus_id,
};
#[cfg(feature = "savestates")]
use crate::emulator::save::{load_state_from, save_state_to};
use crate::keymap::KeyEvent;
use crate::mac::compact::bus::CompactMacBus;
use crate::mac::machine_bus::{MachineBus020, MachineBus040};
use crate::mac::macii::bus::MacIIBus;
use crate::mac::maclc::lc_475_bus::Lc475Bus;
use crate::mac::maclc::lc_i_bus::LcIBus;
use crate::mac::maclc::lc_iii_bus::LcIIIBus;
use crate::mac::maclc::pds_iie::IieCardSettings;
use crate::mac::maclc::pds_iie::disk::DiskII;
use crate::mac::portable::bus::MacPortableBus;
use crate::mac::quadra::bus::Quadra700Bus;
use crate::mac::quadra::q800_bus::Quadra800Bus;
use crate::mac::scc::Scc;
use crate::mac::scsi::target::ScsiTargetEvent;
use crate::mac::serial_bridge::{SccBridge, SerialBridgeStatus};
use crate::mac::swim::drive::DriveType;
use crate::mac::{
    AbsoluteMouse, ExtraROMs, MacModel, MacMonitor, NubusCardConfig, NubusDeviceKind,
};
use crate::renderer::AudioProvider;
use crate::renderer::FrameReceiver;
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

use anyhow::{Result, anyhow, bail};
use log::*;
use std::fmt;

use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::emulator::comm::{EmulatorSpeed, UserMessageType};
use crate::mac::scsi::bus::ScsiBus;
use comm::{
    Breakpoint, EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver,
    EmulatorStatus, InputRecording, MemoryNotifierRange,
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

/// A target on the bus of the machine, or else on the CPU
macro_rules! machine_target {
    ($cpu:ident, $get:ident, bus . $($rest:tt)*) => { $cpu.bus.$get().$($rest)* };
    ($cpu:ident, $get:ident, $($rest:tt)*) => { $cpu.$($rest)* };
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
                        Self::Portable(inner) => &inner.$($ref_target)*,
                        Self::MacII(inner) => &machine_target!(inner, macii, $($ref_target)*),
                        Self::MacII30(inner) => &machine_target!(inner, macii, $($ref_target)*),
                        Self::MacLC(inner) => &machine_target!(inner, lc_i, $($ref_target)*),
                        Self::MacLCII(inner) => &machine_target!(inner, lc_i, $($ref_target)*),
                        Self::MacLCIII(inner) => &machine_target!(inner, lc_iii, $($ref_target)*),
                        Self::Quadra700(inner) => &machine_target!(inner, quadra700, $($ref_target)*),
                        Self::LC475(inner) => &machine_target!(inner, lc475, $($ref_target)*),
                        Self::Quadra800(inner) => &machine_target!(inner, quadra800, $($ref_target)*),
                    }
                }
            )*

            // Generate mutable reference methods
            $(
                pub fn $mut_ref_method(&mut self) -> $mut_ref_ret {
                    match self {
                        Self::Compact(inner) => &mut inner.$($mut_ref_target)*,
                        Self::Portable(inner) => &mut inner.$($mut_ref_target)*,
                        Self::MacII(inner) => &mut machine_target!(inner, macii_mut, $($mut_ref_target)*),
                        Self::MacII30(inner) => &mut machine_target!(inner, macii_mut, $($mut_ref_target)*),
                        Self::MacLC(inner) => &mut machine_target!(inner, lc_i_mut, $($mut_ref_target)*),
                        Self::MacLCII(inner) => &mut machine_target!(inner, lc_i_mut, $($mut_ref_target)*),
                        Self::MacLCIII(inner) => &mut machine_target!(inner, lc_iii_mut, $($mut_ref_target)*),
                        Self::Quadra700(inner) => &mut machine_target!(inner, quadra700_mut, $($mut_ref_target)*),
                        Self::LC475(inner) => &mut machine_target!(inner, lc475_mut, $($mut_ref_target)*),
                        Self::Quadra800(inner) => &mut machine_target!(inner, quadra800_mut, $($mut_ref_target)*),
                    }
                }
            )*

            // Generate immutable method calls
            $(
                pub fn $immut_call_method(&self $(, $immut_arg: $immut_arg_ty)*) -> $immut_call_ret {
                    match self {
                        Self::Compact(inner) => inner.$($immut_call_target)*,
                        Self::Portable(inner) => inner.$($immut_call_target)*,
                        Self::MacII(inner) => machine_target!(inner, macii, $($immut_call_target)*),
                        Self::MacII30(inner) => machine_target!(inner, macii, $($immut_call_target)*),
                        Self::MacLC(inner) => machine_target!(inner, lc_i, $($immut_call_target)*),
                        Self::MacLCII(inner) => machine_target!(inner, lc_i, $($immut_call_target)*),
                        Self::MacLCIII(inner) => machine_target!(inner, lc_iii, $($immut_call_target)*),
                        Self::Quadra700(inner) => machine_target!(inner, quadra700, $($immut_call_target)*),
                        Self::LC475(inner) => machine_target!(inner, lc475, $($immut_call_target)*),
                        Self::Quadra800(inner) => machine_target!(inner, quadra800, $($immut_call_target)*),
                    }
                }
            )*

            // Generate mutable method calls
            $(
                pub fn $mut_call_method(&mut self $(, $mut_arg: $mut_arg_ty)*) -> $mut_call_ret {
                    match self {
                        Self::Compact(inner) => inner.$($mut_call_target)*,
                        Self::Portable(inner) => inner.$($mut_call_target)*,
                        Self::MacII(inner) => machine_target!(inner, macii_mut, $($mut_call_target)*),
                        Self::MacII30(inner) => machine_target!(inner, macii_mut, $($mut_call_target)*),
                        Self::MacLC(inner) => machine_target!(inner, lc_i_mut, $($mut_call_target)*),
                        Self::MacLCII(inner) => machine_target!(inner, lc_i_mut, $($mut_call_target)*),
                        Self::MacLCIII(inner) => machine_target!(inner, lc_iii_mut, $($mut_call_target)*),
                        Self::Quadra700(inner) => machine_target!(inner, quadra700_mut, $($mut_call_target)*),
                        Self::LC475(inner) => machine_target!(inner, lc475_mut, $($mut_call_target)*),
                        Self::Quadra800(inner) => machine_target!(inner, quadra800_mut, $($mut_call_target)*),
                    }
                }
            )*
        }
    };
}

/// Low memory globals the absolute mouse mode reads and writes, which are
/// logical addresses: MTemp, Y coordinate (16 bit, signed)
const ADDR_MTEMP_Y: Address = 0x0828;
/// MTemp, X coordinate (16 bit, signed)
const ADDR_MTEMP_X: Address = 0x082A;
/// RawMouse, Y coordinate (16 bit, signed)
const ADDR_RAWMOUSE_Y: Address = 0x082C;
/// RawMouse, X coordinate (16 bit, signed)
const ADDR_RAWMOUSE_X: Address = 0x082E;
/// CrsrNew
const ADDR_CRSRNEW: Address = 0x08CE;

/// Least time between two looks at the memory notifiers while running. Debugging
/// mostly happens stopped, where each step or stop looks at them regardless.
const MEMORY_NOTIFY_INTERVAL: Duration = Duration::from_millis(200);

/// Size of the blocks that large accesses to memory run in
const MEMORY_BLOCK: usize = 1024 * 1024;

/// A range of memory the frontend wants to be told about
struct MemoryNotifier {
    range: MemoryNotifierRange,
    /// What was last sent: nothing yet if None
    last: Option<(Address, Vec<Option<u8>>)>,
}

/// What can change memory views without a write to memory: translations, the
/// current mode, the map of physical addresses
#[derive(Clone, Copy, PartialEq, Eq)]
struct MemoryMapState {
    mmu_generation: u32,
    bus_generation: u32,
    supervisor: bool,
}

/// Reads `len` bytes from `addr` on with `read`, a run at a time, None for holes
fn read_memory_range(
    addr: Address,
    len: usize,
    mut read: impl FnMut(Address, &mut [u8]) -> InspectRun,
) -> Vec<Option<u8>> {
    let mut data = Vec::with_capacity(len);
    let mut buf = vec![0; len];
    while data.len() < len {
        let at = addr.wrapping_add(data.len() as Address);
        match read(at, &mut buf[..len - data.len()]) {
            InspectRun::Data(n) => data.extend(buf[..n].iter().map(|&b| Some(b))),
            InspectRun::Hole(n) => data.resize(data.len() + n, None),
        }
    }
    data
}

/// Looks for `needle` from `start` up to `last` included, with `read`. A match
/// doesn't span a hole.
fn search_memory(
    start: Address,
    last: Address,
    needle: &[u8],
    mut read: impl FnMut(Address, &mut [u8]) -> InspectRun,
) -> Option<Address> {
    if needle.is_empty() || needle.len() > MEMORY_BLOCK {
        return None;
    }
    let mut buf = vec![0; MEMORY_BLOCK + needle.len()];
    // What is kept in the buffer of the bytes before `at`
    let mut kept = 0;
    let mut at = u64::from(start);
    while at <= u64::from(last) {
        let room = (u64::from(last) - at + 1).min(MEMORY_BLOCK as u64) as usize;
        match read(at as Address, &mut buf[kept..kept + room]) {
            InspectRun::Hole(n) => {
                kept = 0;
                at += n as u64;
            }
            InspectRun::Data(n) => {
                let window = &buf[..kept + n];
                if let Some(pos) = window.windows(needle.len()).position(|w| w == needle) {
                    return Some((at - kept as u64 + pos as u64) as Address);
                }
                let keep = (needle.len() - 1).min(window.len());
                buf.copy_within(kept + n - keep..kept + n, 0);
                kept = keep;
                at += n as u64;
            }
        }
    }
    None
}

/// Emulator config. Basically an abstraction on top of the CPU for multiple different model groups
/// that provides access to the inner components by the emulator runner through dynamic dispatch.
#[derive(Serialize, Deserialize)]
enum EmulatorConfig {
    /// Compact series - Mac 128K, 512K, Plus, SE, Classic
    Compact(Box<CpuM68000<CompactMacBus>>),
    /// Macintosh Portable
    Portable(Box<CpuM68000<MacPortableBus>>),
    /// Macintosh II, with the AMU or a 68851
    MacII(Box<CpuM68020<MachineBus020>>),
    /// Macintosh SE/30 and 68030-based Macintosh IIs
    MacII30(Box<CpuM68030<MachineBus020>>),
    /// Macintosh LC, with or without its FPU card
    MacLC(Box<CpuM68020<MachineBus020>>),
    /// Macintosh LC II, with or without an FPU
    MacLCII(Box<CpuM68030<MachineBus020>>),
    /// Macintosh LC III, with or without an FPU
    MacLCIII(Box<CpuM68030<MachineBus020>>),
    /// Macintosh Quadra 700 (68040)
    Quadra700(Box<CpuM68040<MachineBus040>>),
    /// Macintosh LC 475: a 68LC040, or a 68040 in its socket
    LC475(Box<CpuM68040<MachineBus040>>),
    /// Macintosh Quadra 800 (68040)
    Quadra800(Box<CpuM68040<MachineBus040>>),
}

dispatch! {
    immutable_refs {
        fn scc(&self) -> &Scc { bus.scc }
        fn cpu_regs(&self) -> &RegisterFile { regs }
    }

    mutable_refs {
        fn scc_mut(&mut self) -> &mut Scc { bus.scc }
        fn cpu_regs_mut(&mut self) -> &mut RegisterFile { regs }
    }

    immutable_calls {
        fn scsi(&self) -> &ScsiBus { bus.scsi.bus() }
        fn model(&self) -> MacModel { bus.model() }
        fn cpu_has_pmmu(&self) -> bool { has_pmmu() }
        fn cpu_cycles(&self) -> Ticks { cycles }
        fn cpu_breakpoints(&self) -> &[Breakpoint] { breakpoints() }
        fn cpu_address_mask(&self) -> Address { address_mask() }
        fn ram_size(&self) -> usize { bus.ram.len() }
        fn cpu_mmu_generation(&self) -> u32 { mmu_generation() }
        fn bus_inspect_generation(&self) -> u32 { bus.inspect_generation() }
        fn cpu_get_step_over(&self) -> Option<Address> { get_step_over() }
        fn speed(&self) -> EmulatorSpeed { bus.speed }
        fn effective_speed(&self) -> f64 { bus.get_effective_speed() }
        fn debug_properties(&self) -> DebuggableProperties { bus.get_debug_properties() }
    }

    mutable_calls {
        fn scsi_mut(&mut self) -> &mut ScsiBus { bus.scsi.bus_mut() }
        fn scsi_settle(&mut self) -> () { bus.scsi.settle() }
        fn set_speed(&mut self, speed: EmulatorSpeed) -> () { bus.set_speed(speed) }
        fn set_overclock(&mut self, bus_frequency: Option<u64>) -> () { bus.set_overclock(bus_frequency) }
        fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> { bus.set_audio_provider(provider) }

        fn cpu_tick(&mut self, ticks: Ticks) -> Result<Ticks> { tick(ticks, ()) }
        fn cpu_set_breakpoint(&mut self, bp: Breakpoint) -> () { set_breakpoint(bp) }
        fn cpu_breakpoints_mut(&mut self) -> &mut Vec<Breakpoint> { breakpoints_mut() }
        fn cpu_clear_breakpoint(&mut self, bp: Breakpoint) -> () { clear_breakpoint(bp) }
        fn cpu_enable_history(&mut self, v: bool) -> () { enable_history(v) }
        fn cpu_enable_systrap_history(&mut self, v: bool) -> () { enable_systrap_history(v) }
        fn cpu_set_cache_flush_workaround(&mut self, v: bool) -> () { set_cache_flush_workaround(v) }
        fn cpu_set_vm_pflush_workaround(&mut self, v: bool) -> () { set_vm_pflush_workaround(v) }
        fn cpu_set_pc(&mut self, pc: Address) -> Result<()> { set_pc(pc) }
        fn cpu_get_clr_breakpoint_hit(&mut self) -> bool { get_clr_breakpoint_hit() }
        fn cpu_read_history(&mut self) -> Option<&[HistoryEntry]> { read_history() }
        fn cpu_read_systrap_history(&mut self) -> Option<&[SystrapHistoryEntry]> { read_systrap_history() }
        fn cpu_run_steps(&mut self, steps: u32) -> Result<(u32, bool)> { run_steps(steps) }
        fn cpu_prefetch_refill(&mut self) -> Result<()> { prefetch_refill() }
        fn cpu_reset(&mut self) -> Result<()> { reset() }

        fn bus_reset(&mut self) -> Result<bool> { bus.reset(true) }
        fn bus_take_reset_request(&mut self) -> bool { bus.take_reset_request() }
        fn after_deserialize(&mut self) -> () { bus.after_deserialize() }
        fn cpu_inspect_read(&mut self, addr: Address, phys: bool) -> Option<Byte> { inspect_read(addr, phys) }
        fn cpu_inspect_write(&mut self, addr: Address, val: Byte, phys: bool) -> Option<()> { inspect_write(addr, val, phys) }
        fn cpu_inspect_read_word(&mut self, addr: Address, phys: bool) -> Option<u16> { inspect_read_word(addr, phys) }
        fn cpu_inspect_write_word(&mut self, addr: Address, val: u16, phys: bool) -> Option<()> { inspect_write_word(addr, val, phys) }
        fn cpu_inspect_read_bulk(&mut self, addr: Address, view: InspectView, out: &mut [Byte]) -> InspectRun { inspect_read_bulk(addr, view, out) }
        fn cpu_inspect_watch(&mut self, ranges: &[(Address, u32, InspectView)]) -> () { inspect_watch(ranges) }
        fn cpu_inspect_take_watch_hit(&mut self) -> bool { inspect_take_watch_hit() }

        fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) -> () { bus.mouse_update_rel(relx, rely, button) }
        fn mouse_update_abs(&mut self, x: u16, y: u16, rawmouse: (u16, u16)) -> Option<AbsoluteMouse> { bus.mouse_update_abs(x, y, rawmouse) }
        fn set_mouse_mode(&mut self, mode: MouseMode) -> () { bus.set_mouse_mode(mode) }
        fn keyboard_event(&mut self, ke: KeyEvent) -> () { bus.keyboard_event(ke) }
        fn input_release_all(&mut self) -> () { bus.input_release_all() }
        fn progkey(&mut self) -> () { bus.progkey() }
        fn video_blank(&mut self) -> Result<()> { bus.video_blank() }
        fn video_frames(&mut self) -> FrameReceiver { bus.video_frames() }

        fn bus_ops(&mut self, id: &str) -> Option<&mut dyn BusOps> { bus.bus_ops(id) }

        fn persist_pram(&mut self, path: &Path) -> () { bus.persist_pram(path) }
        fn set_datetime(&mut self, dt: chrono::NaiveDateTime) -> () { bus.set_datetime(dt) }
    }
}

/// An interface that allows sub-components to access emulator state (such as the speed setting)
pub trait EmuContext {
    fn speed(&self) -> EmulatorSpeed;
    fn bus_frequency(&self) -> Ticks;
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
    /// The Apple IIe Card was in front at the last check
    iie_in_front: bool,
    model: MacModel,
    record_input: Option<InputRecording>,
    replay_input: VecDeque<(Ticks, EmulatorCommand)>,
    peripheral_debug: bool,
    /// Serial bridges for SCC channels (index 0 = Channel A, index 1 = Channel B)
    serial_bridges: [Option<SccBridge>; 2],
    audio_provider: Option<Arc<Mutex<dyn AudioProvider>>>,
    /// Ranges of memory the frontend wants to be told about, by id
    memory_notifiers: BTreeMap<u32, MemoryNotifier>,
    /// Map state the watched ranges were set for
    memory_map_state: Option<MemoryMapState>,
    /// Last look at the memory notifiers
    memory_notify_time: Instant,
    /// The temporary files a restored state made, which its devices carry on from
    temp_files: Vec<PathBuf>,
    /// The list of buses as it was last sent to the frontend
    buses: Arc<Vec<crate::devices::Bus>>,
    /// The status last sent of each slot of that list, in its order
    device_statuses: Vec<Option<FunctionStatus>>,
}

impl Emulator {
    pub fn new(
        rom: &[u8],
        extra_roms: &[ExtraROMs],
        model: MacModel,
    ) -> Result<(Self, FrameReceiver)> {
        Self::new_with_extra(
            rom,
            extra_roms,
            model,
            None,
            MouseMode::default(),
            None,
            None,
            false,
            false,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::large_stack_frames)]
    pub fn new_with_extra(
        rom: &[u8],
        extra_roms: &[ExtraROMs],
        model: MacModel,
        monitor: Option<MacMonitor>,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
        override_fdd_type: Option<DriveType>,
        pmmu_enabled: bool,
        fpu_enabled: bool,
        vram_size: Option<usize>,
        shared_dir: Option<PathBuf>,
        iie_card: Option<IieCardSettings>,
    ) -> Result<(Self, FrameReceiver)> {
        // Set up channels
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();

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
                let bus = CompactMacBus::new(model, rom, extension_rom, mouse_mode, ram_size);
                let cpu = Box::new(CpuM68000::new(bus, FPU_NONE));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::Compact(cpu)
            }
            MacModel::Portable | MacModel::Portable15MB => {
                assert!(!pmmu_enabled, "PMMU not available on compact models");

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = MacPortableBus::new(model, rom, extension_rom, mouse_mode, ram_size);
                let cpu = Box::new(CpuM68000::new(bus, FPU_NONE));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::Portable(cpu)
            }
            MacModel::MacII | MacModel::MacIIFDHD => {
                assert!(override_fdd_type.is_none());

                // Find display card ROM and the selected card type
                let Some((kind, mdcrom)) = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::MDC12(data) => Some((NubusDeviceKind::Mdc12, *data)),
                    ExtraROMs::Toby(data) => Some((NubusDeviceKind::Toby, *data)),
                    _ => None,
                }) else {
                    bail!("Macintosh II requires display card ROM")
                };
                let nubus = [NubusCardConfig {
                    slot: 0x9,
                    kind,
                    rom: mdcrom,
                }];

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU. A 68851 takes the place of the AMU.
                let bus = MacIIBus::new(
                    model,
                    rom,
                    &nubus,
                    extension_rom,
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                    !pmmu_enabled,
                );
                let mut cpu = Box::new(CpuM68020::new(MachineBus020::MacII(bus), FPU_M68881));
                if pmmu_enabled {
                    cpu.add_68851();
                }
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::MacII(cpu)
            }
            MacModel::MacIIx | MacModel::MacIIcx => {
                assert!(override_fdd_type.is_none());

                // Find display card ROM and the selected card type
                let Some((kind, mdcrom)) = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::MDC12(data) => Some((NubusDeviceKind::Mdc12, *data)),
                    ExtraROMs::Toby(data) => Some((NubusDeviceKind::Toby, *data)),
                    _ => None,
                }) else {
                    bail!("Macintosh II requires display card ROM")
                };
                let nubus = [NubusCardConfig {
                    slot: 0x9,
                    kind,
                    rom: mdcrom,
                }];

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = MacIIBus::new(
                    model,
                    rom,
                    &nubus,
                    extension_rom,
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                    false,
                );
                let cpu = Box::new(CpuM68030::new(MachineBus020::MacII(bus), FPU_M68882));
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
                let nubus = [NubusCardConfig {
                    slot: 0xE,
                    kind: NubusDeviceKind::SE30Video,
                    rom: vrom,
                }];

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = MacIIBus::new(
                    model,
                    rom,
                    &nubus,
                    extension_rom,
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                    false,
                );
                let cpu = Box::new(CpuM68030::new(MachineBus020::MacII(bus), FPU_M68882));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::MacII30(cpu)
            }
            MacModel::LC => {
                assert!(!pmmu_enabled, "PMMU not available on Macintosh LC");
                assert!(override_fdd_type.is_none());

                // Initialize bus and CPU
                // The FPU is an option on an expansion card
                let mut bus = LcIBus::new(
                    model, rom, mouse_mode, ram_size, monitor, vram_size, iie_card,
                );
                bus.set_security_jumper(iie_card.is_some_and(|s| s.security_jumper));
                let fpu = if fpu_enabled { FPU_M68882 } else { FPU_NONE };
                EmulatorConfig::MacLC(Box::new(CpuM68020::new(MachineBus020::LcI(bus), fpu)))
            }
            MacModel::LCII => {
                assert!(override_fdd_type.is_none());

                // Initialize bus and CPU
                // The FPU is an option on an expansion card
                let mut bus = LcIBus::new(
                    model, rom, mouse_mode, ram_size, monitor, vram_size, iie_card,
                );
                bus.set_security_jumper(iie_card.is_some_and(|s| s.security_jumper));
                let fpu = if fpu_enabled { FPU_M68882 } else { FPU_NONE };
                EmulatorConfig::MacLCII(Box::new(CpuM68030::new(MachineBus020::LcI(bus), fpu)))
            }
            MacModel::LCIII => {
                assert!(override_fdd_type.is_none());

                // Initialize bus and CPU
                // The FPU is an option on an expansion card
                let mut bus = LcIIIBus::new(
                    model, rom, mouse_mode, ram_size, monitor, vram_size, iie_card,
                );
                bus.set_security_jumper(iie_card.is_some_and(|s| s.security_jumper));
                let fpu = if fpu_enabled { FPU_M68882 } else { FPU_NONE };
                EmulatorConfig::MacLCIII(Box::new(CpuM68030::new(MachineBus020::LcIII(bus), fpu)))
            }
            MacModel::Quadra700 => {
                assert!(override_fdd_type.is_none());

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let bus = Quadra700Bus::new(
                    model,
                    rom,
                    extension_rom,
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                );
                let cpu = Box::new(CpuM68040::new(MachineBus040::Quadra700(bus), FPU_M68040));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::Quadra700(cpu)
            }
            MacModel::LC475 => {
                assert!(override_fdd_type.is_none());

                let bus = Lc475Bus::new(
                    model,
                    rom,
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                    vram_size,
                    iie_card,
                );
                // The CPU is socketed, and some owners fit a 68040
                let fpu = if fpu_enabled { FPU_M68040 } else { FPU_NONE };
                EmulatorConfig::LC475(Box::new(CpuM68040::new(MachineBus040::Lc475(bus), fpu)))
            }
            MacModel::Quadra800 => {
                assert!(override_fdd_type.is_none());

                let bus = Quadra800Bus::new(
                    model,
                    rom,
                    monitor.unwrap_or_default(),
                    mouse_mode,
                    ram_size,
                    vram_size,
                );
                let cpu = CpuM68040::new(MachineBus040::Quadra800(bus), FPU_M68040);
                EmulatorConfig::Quadra800(Box::new(cpu))
            }
        };

        config.scsi_mut().set_shared_dir(shared_dir);
        config.cpu_reset()?;
        let frame_recv = config.video_frames();

        let mut emu = Self {
            config,
            command_recv: cmdr,
            command_sender: cmds,
            event_sender: statuss,
            event_recv: statusr,
            run: false,
            last_update: Instant::now(),
            iie_in_front: false,
            model,
            record_input: None,
            replay_input: VecDeque::default(),
            peripheral_debug: false,
            serial_bridges: [None, None],
            audio_provider: None,
            memory_notifiers: BTreeMap::new(),
            memory_map_state: None,
            memory_notify_time: Instant::now(),
            temp_files: vec![],
            buses: Arc::default(),
            device_statuses: vec![],
        };
        emu.add_default_devices(override_fdd_type)?;
        let buses = emu.buses();
        emu.send_buses(buses)?;
        emu.status_update()?;

        for ch in crate::mac::scc::SccCh::iter() {
            emu.event_sender
                .send(EmulatorEvent::SerialBridgeStatus(ch, None))?;
        }

        Ok((emu, frame_recv))
    }

    /// The temporary files that a restored state made: the images its devices carry on from.
    /// For the frontend to remove when it is done with the machine.
    pub fn temp_files(&self) -> &[PathBuf] {
        &self.temp_files
    }

    /// Restores a saved emulator state into a new Emulator instance
    #[cfg(feature = "savestates")]
    pub fn load_state<P: AsRef<Path>, PT: AsRef<Path>>(
        path: P,
        tmpdir: PT,
    ) -> Result<(Self, FrameReceiver)> {
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();
        let time = Instant::now();

        let fstr = path
            .as_ref()
            .file_name()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let f = File::open(path)?;

        let (mut config, temp_files) = load_state_from(f, tmpdir)?;
        config.after_deserialize();
        let frame_recv = config.video_frames();
        // The keys and buttons held down when the state was saved are not
        // any more, or we cannot know: release them rather than leave them
        // stuck.
        config.input_release_all();

        let model = config.model();

        let mut emu = Self {
            config,
            command_recv: cmdr,
            command_sender: cmds,
            event_sender: statuss,
            event_recv: statusr,
            run: false,
            last_update: Instant::now(),
            iie_in_front: false,
            model,
            record_input: None,
            replay_input: VecDeque::default(),
            peripheral_debug: false,
            serial_bridges: [None, None],
            audio_provider: None,
            memory_notifiers: BTreeMap::new(),
            memory_map_state: None,
            memory_notify_time: Instant::now(),
            temp_files,
            buses: Arc::default(),
            device_statuses: vec![],
        };
        let buses = emu.buses();
        emu.send_buses(buses)?;
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
        self.config.persist_pram(pram_path);
    }

    /// Sets the RTC to a specific date/time.
    /// This can be used to test date-dependent software behavior (e.g., easter eggs).
    pub fn set_datetime(&mut self, dt: chrono::NaiveDateTime) {
        self.config.set_datetime(dt);
    }

    pub fn create_cmd_sender(&self) -> EmulatorCommandSender {
        self.command_sender.clone()
    }

    pub fn create_event_recv(&self) -> EmulatorEventReceiver {
        self.event_recv.clone()
    }

    /// The Apple IIe Card of the machine, seen through its function, on a machine that has one
    fn iie_card(&mut self) -> Option<&mut dyn AppleIIeCardFunction> {
        let card = match &mut self.config {
            EmulatorConfig::MacLC(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::MacLCII(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::MacLCIII(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::LC475(inner) => inner.bus.iie_card_mut(),
            _ => None,
        };
        match card?.function() {
            FunctionMut::AppleIIeCard(card) => Some(card),
            _ => None,
        }
    }

    /// The slot, on the disk port of the Apple IIe Card, of the `n`th drive of a device model
    fn iie_drive_slot(&mut self, model: DeviceModel, n: usize) -> Option<&'static str> {
        let bus = self.config.bus_ops(bus_id::IIE_DISK)?;
        DiskII::SLOT_IDS
            .into_iter()
            .filter(|slot| bus.device(slot).is_some_and(|d| d.info().model == model))
            .nth(n)
    }

    /// Sends the frontend the state of the machine and of its devices, as it is now. It is sent
    /// every 500 ms while the machine runs and after each command. A frontend that calls the
    /// emulator's methods itself calls this after it changed a device.
    pub fn status_update(&mut self) -> Result<()> {
        self.send_ejected_floppies()?;
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
                        .send(EmulatorEvent::UserMessage(
                            UserMessageType::Notice,
                            format!("CD-ROM #{} ejected", id),
                        ))
                        .unwrap();
                }
                Some(ScsiTargetEvent::LoadFailed(msg)) => {
                    self.event_sender
                        .send(EmulatorEvent::UserMessage(
                            UserMessageType::Error,
                            format!("SCSI ID #{}: {}", id, msg),
                        ))
                        .unwrap();
                }
                #[cfg(feature = "printer")]
                Some(ScsiTargetEvent::PageSaved(f)) => {
                    self.event_sender
                        .send(EmulatorEvent::UserMessage(
                            UserMessageType::Notice,
                            format!("LaserWriter: page saved as '{}'", f),
                        ))
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
                model: self.model,
                has_pmmu: self.config.cpu_has_pmmu(),
                speed: self.config.speed(),
                ram_size: self.config.ram_size(),
                effective_speed: self.config.effective_speed(),
            })))?;
        self.devices_update()?;

        // Memory notifiers. A stopped machine gets all of them looked at, so that
        // whatever happened without a notice, such as changes in the page tables
        // with no flush, shows up. While running, memory_notifiers_tick() handles
        // changes, and only what follows the PC moves here.
        if !self.memory_notifiers.is_empty() {
            self.memory_notifiers_set_watch();
            if self.run {
                self.memory_notifiers_send(true)?;
            } else {
                self.memory_map_state = Some(self.memory_map_state());
                self.config.cpu_inspect_take_watch_hit();
                self.memory_notifiers_send(false)?;
            }
        }

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

    /// What can change memory views now, without a write to memory
    fn memory_map_state(&self) -> MemoryMapState {
        MemoryMapState {
            mmu_generation: self.config.cpu_mmu_generation(),
            bus_generation: self.config.bus_inspect_generation(),
            supervisor: self.config.cpu_regs().sr.supervisor(),
        }
    }

    /// Where a notifier's range starts now
    fn memory_range_addr(&self, range: &MemoryNotifierRange) -> Address {
        if range.follow_pc {
            self.config.cpu_regs().pc
        } else {
            range.addr
        }
    }

    /// Tells the CPU what to watch for the notifiers, as things stand
    fn memory_notifiers_watch(&mut self) {
        self.memory_map_state = Some(self.memory_map_state());
        self.memory_notifiers_set_watch();
    }

    /// Gives the CPU the ranges of the notifiers to watch
    fn memory_notifiers_set_watch(&mut self) {
        let ranges: Vec<_> = self
            .memory_notifiers
            .values()
            .map(|n| (self.memory_range_addr(&n.range), n.range.len, n.range.view))
            .collect();
        self.config.cpu_inspect_watch(&ranges);
    }

    /// Reads the ranges of the notifiers, or only those that follow the PC, and
    /// sends those that changed
    fn memory_notifiers_send(&mut self, only_follow_pc: bool) -> Result<()> {
        let mut notifiers = std::mem::take(&mut self.memory_notifiers);
        for (&id, notifier) in &mut notifiers {
            let range = notifier.range;
            if only_follow_pc && !range.follow_pc {
                continue;
            }
            let addr = self.memory_range_addr(&range);
            let config = &mut self.config;
            let data = read_memory_range(addr, range.len as usize, |at, out| {
                config.cpu_inspect_read_bulk(at, range.view, out)
            });
            if notifier
                .last
                .as_ref()
                .is_none_or(|(a, d)| *a != addr || *d != data)
            {
                self.event_sender.send(EmulatorEvent::MemoryNotify {
                    id,
                    addr,
                    data: data.clone(),
                })?;
                notifier.last = Some((addr, data));
            }
        }
        self.memory_notifiers = notifiers;
        Ok(())
    }

    /// Looks if any notifier needs to be told something, while running
    fn memory_notifiers_tick(&mut self) -> Result<()> {
        if self.memory_notify_time.elapsed() < MEMORY_NOTIFY_INTERVAL {
            return Ok(());
        }
        self.memory_notify_time = Instant::now();
        let map_changed = self.memory_map_state != Some(self.memory_map_state());
        if map_changed {
            self.memory_notifiers_watch();
        }
        if self.config.cpu_inspect_take_watch_hit() || map_changed {
            self.memory_notifiers_send(false)?;
        }
        Ok(())
    }

    /// Writes a range of memory to a file
    fn memory_export(
        &mut self,
        path: &Path,
        start: Address,
        len: usize,
        view: InspectView,
    ) -> Result<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut buf = vec![0; MEMORY_BLOCK];
        let mut done = 0;
        while done < len {
            let at = start.wrapping_add(done as Address);
            let room = (len - done).min(MEMORY_BLOCK);
            match self
                .config
                .cpu_inspect_read_bulk(at, view, &mut buf[..room])
            {
                InspectRun::Data(n) => {
                    std::io::Write::write_all(&mut f, &buf[..n])?;
                    done += n;
                }
                InspectRun::Hole(n) => {
                    std::io::Write::write_all(&mut f, &vec![0; n])?;
                    done += n;
                }
            }
        }
        std::io::Write::flush(&mut f)?;
        Ok(())
    }

    /// Reports a new position of the mouse in absolute mode to the OS, in its
    /// low memory globals. If they have no translation at the moment, this
    /// position is skipped.
    fn mouse_update_abs(&mut self, x: u16, y: u16) {
        let (Some(old_x), Some(old_y)) = (
            self.config.cpu_inspect_read_word(ADDR_RAWMOUSE_X, false),
            self.config.cpu_inspect_read_word(ADDR_RAWMOUSE_Y, false),
        ) else {
            return;
        };
        let Some(mouse) = self.config.mouse_update_abs(x, y, (old_x, old_y)) else {
            return;
        };
        self.config
            .cpu_inspect_write_word(ADDR_MTEMP_X, mouse.mtemp.0, false);
        self.config
            .cpu_inspect_write_word(ADDR_MTEMP_Y, mouse.mtemp.1, false);
        self.config
            .cpu_inspect_write_word(ADDR_RAWMOUSE_X, mouse.rawmouse.0, false);
        self.config
            .cpu_inspect_write_word(ADDR_RAWMOUSE_Y, mouse.rawmouse.1, false);
        self.config.cpu_inspect_write(ADDR_CRSRNEW, 1, false);
    }

    /// Steps the emulator by one instruction.
    fn step(&mut self) -> Result<()> {
        let (_, reset) = self.config.cpu_run_steps(1)?;
        self.after_steps(reset)
    }

    /// What follows the instructions of EmulatorConfig::cpu_run_steps()
    fn after_steps(&mut self, reset: bool) -> Result<()> {
        let mut stop_break = false;
        if reset {
            // As for the reset command: the bus first, for the overlay
            self.config.bus_reset()?;
            self.config.cpu_reset()?;
            self.config.video_blank()?;
        }

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

    /// Gives the machine the sound output of the host, and each CD-ROM drive too, for audio CDs
    pub fn set_audio_provider(&mut self, provider: Arc<Mutex<dyn AudioProvider>>) -> Result<()> {
        self.config
            .set_audio_provider(&mut *provider.lock().unwrap())?;
        for bus in self.buses() {
            for slot in &bus.slots {
                if let Ok(FunctionMut::Cdrom(cdrom)) = self.device_function(&bus.id, &slot.id) {
                    cdrom.set_audio_provider(&mut *provider.lock().unwrap())?;
                }
            }
        }
        self.audio_provider = Some(provider);
        Ok(())
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

    fn poll_localtalk_bridge(&mut self) {
        // Demand-driven LocalTalk polling: poll immediately when the
        // Mac writes TX data or re-enables RX, so the byte-stream
        // path delivers frames promptly (the driver's CTS poll
        // window is too short for the next tick batch). The bridge's
        // frames wait until the guest can receive them, as below.
        if self
            .config
            .scc_mut()
            .take_localtalk_poll_needed(crate::mac::scc::SccCh::B)
            && let Some(ref mut bridge) = self.serial_bridges[1]
            && bridge.is_localtalk()
        {
            // Propagate SCC state to LocalTalk bridge
            let sdlc_addr = self.config.scc().sdlc_address(crate::mac::scc::SccCh::B);
            bridge.set_node_address(sdlc_addr);
            bridge.set_address_search_mode(
                self.config
                    .scc()
                    .is_address_search_mode(crate::mac::scc::SccCh::B),
            );

            // Byte-stream TX path: the bridge extracts complete
            // packets and returns immediate responses (e.g. the
            // CTS for a unicast RTS)
            if self.config.scc().has_tx_data(crate::mac::scc::SccCh::B) {
                let tx_data = self.config.scc_mut().take_tx(crate::mac::scc::SccCh::B);
                let replies = bridge.write_from_scc(&tx_data);
                for reply in replies {
                    self.config
                        .scc_mut()
                        .push_rx_frame(crate::mac::scc::SccCh::B, reply);
                }
            }
            // The frame boundary, as a resynchronization point: this
            // polls after each step, so no byte of the next frame
            // has been taken yet
            if self
                .config
                .scc_mut()
                .take_tx_frame_ended(crate::mac::scc::SccCh::B)
            {
                for reply in bridge.end_tx_frame() {
                    self.config
                        .scc_mut()
                        .push_rx_frame(crate::mac::scc::SccCh::B, reply);
                }
            }

            // Now poll for incoming data and pending CTS
            bridge.poll();
            if let Some(frame) = self
                .config
                .scc_mut()
                .take_unread_frame(crate::mac::scc::SccCh::B)
            {
                bridge.return_frame(frame);
            }
            if self
                .config
                .scc()
                .is_rx_ready_for_data(crate::mac::scc::SccCh::B)
            {
                let rx_data = bridge.read_to_scc();
                if !rx_data.is_empty() {
                    self.config
                        .scc_mut()
                        .push_rx(crate::mac::scc::SccCh::B, &rx_data);
                }
            }
        }
    }

    #[inline(always)]
    fn try_step(&mut self) {
        if let Err(e) = self.step() {
            self.step_failed(&e);
        }
    }

    fn step_failed(&mut self, e: &anyhow::Error) {
        self.run = false;
        self.user_error(&format!(
            "Emulator halted: Uncaught CPU stepping error at PC {:08X}\nSee log for details",
            self.config.cpu_regs().pc,
        ));
        log::error!(
            "Emulator halted: Uncaught CPU stepping error at PC {:08X}: {}",
            self.config.cpu_regs().pc,
            e
        );
        let _ = self.status_update();
    }

    pub fn get_cycles(&self) -> Ticks {
        self.config.cpu_cycles()
    }

    #[cfg(feature = "savestates")]
    fn save_state(&mut self, p: &Path, screenshot: Option<Vec<u8>>) -> Result<()> {
        let mut f = File::create(p)?;
        let time = Instant::now();

        save_state_to(&f, &mut self.config, screenshot)?;

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
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
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

                        self.mouse_update_abs(x, y);
                    }
                    EmulatorCommand::SetMouseMode(mode) => {
                        log::info!("Mouse mode: {:?}", mode);
                        self.config.set_mouse_mode(mode);
                    }
                    EmulatorCommand::Quit => {
                        info!("Emulator terminating");
                        self.flush_pending_writebacks(true);
                        self.config.video_blank()?;
                        return Ok(0);
                    }
                    EmulatorCommand::InsertIieDisk(drive, path) => {
                        let result = match self.iie_drive_slot(DeviceModel::Apple525Drive, drive) {
                            Some(slot) => self
                                .floppy_in(bus_id::IIE_DISK, slot)
                                .and_then(|floppy| floppy.load(&path, true)),
                            None if self.iie_card().is_none() => {
                                Err(anyhow!("No Apple IIe Card installed"))
                            }
                            None => Err(anyhow!(
                                "Cannot insert IIe disk '{}': 5.25\" drive {} is not connected",
                                path.display(),
                                drive + 1
                            )),
                        };
                        if let Err(e) = result {
                            self.user_error(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::SetIieGamePort(port) => {
                        if let Some(card) = self.iie_card() {
                            card.set_game_port(port);
                        }
                    }
                    EmulatorCommand::EjectIieDisk(drive) => {
                        if let Some(slot) = self.iie_drive_slot(DeviceModel::Apple525Drive, drive)
                            && let Ok(floppy) = self.floppy_in(bus_id::IIE_DISK, slot)
                        {
                            floppy.eject();
                        }
                    }
                    EmulatorCommand::InsertIieUniDisk(path) => {
                        let result = match self.iie_drive_slot(DeviceModel::UniDisk35, 0) {
                            Some(slot) => self
                                .floppy_in(bus_id::IIE_DISK, slot)
                                .and_then(|floppy| floppy.load(&path, false)),
                            None if self.iie_card().is_none() => {
                                Err(anyhow!("No Apple IIe Card installed"))
                            }
                            None => Err(anyhow!(
                                "Cannot insert UniDisk 3.5 image '{}': no UniDisk 3.5 is connected",
                                path.display()
                            )),
                        };
                        if let Err(e) = result {
                            self.user_error(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EjectIieUniDisk => {
                        if let Some(slot) = self.iie_drive_slot(DeviceModel::UniDisk35, 0)
                            && let Ok(floppy) = self.floppy_in(bus_id::IIE_DISK, slot)
                        {
                            floppy.eject();
                        }
                    }
                    EmulatorCommand::InsertFloppyImage(bus, slot, img, wp) => {
                        let result = self
                            .floppy_in(&bus, &slot)
                            .and_then(|floppy| floppy.insert_image(img, wp));
                        if let Err(e) = result {
                            self.user_error(&format!("{}: {:#}", self.slot_label(&bus, &slot), e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EjectMedia(bus, slot) => {
                        if let Err(e) = self.eject_media(&bus, &slot) {
                            self.user_error(&format!("{}: {:#}", self.slot_label(&bus, &slot), e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::SetFloppyWriteback(bus, slot, enabled) => {
                        let result = self
                            .floppy_in(&bus, &slot)
                            .and_then(|floppy| floppy.set_writeback(enabled));
                        if let Err(e) = result {
                            self.user_error(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::AddDevice(config) => {
                        if let Err(e) = self.add_device(&config) {
                            let label = self.slot_label(&config.bus, &config.slot);
                            self.user_error(&format!("{}: {:#}", label, e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::RemoveDevice(bus, slot) => {
                        match self.remove_device(&bus, &slot) {
                            Ok(_) => info!("{} #{}: device removed", bus, slot),
                            Err(e) => {
                                self.user_error(&format!(
                                    "{}: {:#}",
                                    self.slot_label(&bus, &slot),
                                    e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::CdromSetFolder(bus, slot, folder) => {
                        if let Err(e) = self.cdrom_set_folder(&bus, &slot, &folder) {
                            self.user_error(&format!("{}: {:#}", self.slot_label(&bus, &slot), e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::LoadMedia(bus, slot, filename, read_only) => {
                        match self.load_media(&bus, &slot, &filename, read_only) {
                            Ok(_) => {
                                info!("{} #{}: image '{}' loaded", bus, slot, filename.display());
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "{}: {:#}",
                                    self.slot_label(&bus, &slot),
                                    e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::BranchDisk(bus, slot, filename) => {
                        match self.branch_disk(&bus, &slot, &filename) {
                            Ok(_) => {
                                info!(
                                    "{} #{}: branched to file '{}'",
                                    bus,
                                    slot,
                                    filename.display()
                                );
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "{}: {:#}",
                                    self.slot_label(&bus, &slot),
                                    e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::SaveFloppy(bus, slot, filename) => {
                        let result = self
                            .floppy_in(&bus, &slot)
                            .and_then(|floppy| floppy.save(&filename));
                        if let Err(e) = result {
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
                    EmulatorCommand::InspectWrite { addr, data, phys } => {
                        for (i, d) in data.into_iter().enumerate() {
                            let addr = addr.wrapping_add(i as Address);
                            if self.config.cpu_inspect_write(addr, d, phys).is_none() {
                                self.user_error(&format!(
                                    "Could not write to address {:08X}",
                                    addr
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::MemoryNotifierSet(id, range) => {
                        let last = self
                            .memory_notifiers
                            .remove(&id)
                            .filter(|n| n.range == range)
                            .and_then(|n| n.last);
                        self.memory_notifiers
                            .insert(id, MemoryNotifier { range, last });
                        self.memory_notifiers_watch();
                        self.memory_notifiers_send(false)?;
                    }
                    EmulatorCommand::MemoryNotifierRemove(id) => {
                        self.memory_notifiers.remove(&id);
                        self.memory_notifiers_watch();
                    }
                    EmulatorCommand::ReadScrapText => {
                        let config = &mut self.config;
                        let text = crate::util::mac::read_scrap_text(|addr| {
                            config.cpu_inspect_read(addr, false)
                        });
                        self.event_sender.send(EmulatorEvent::ScrapText(text))?;
                    }
                    EmulatorCommand::MemorySearch {
                        start,
                        needle,
                        view,
                    } => {
                        let last = self.config.cpu_address_mask();
                        let config = &mut self.config;
                        let result = search_memory(start, last, &needle, |at, out| {
                            config.cpu_inspect_read_bulk(at, view, out)
                        });
                        self.event_sender
                            .send(EmulatorEvent::MemorySearchResult(result))?;
                    }
                    EmulatorCommand::MemoryExport {
                        path,
                        start,
                        len,
                        view,
                    } => {
                        if let Err(e) = self.memory_export(&path, start, len, view) {
                            self.user_error(&format!("Error exporting memory to file: {}", e));
                        }
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
                    EmulatorCommand::SetOverclock(overclock) => {
                        self.config.set_overclock(overclock);
                    }
                    EmulatorCommand::SetCacheFlushWorkaround(on) => {
                        self.config.cpu_set_cache_flush_workaround(on);
                    }
                    EmulatorCommand::SetVmPflushWorkaround(on) => {
                        self.config.cpu_set_vm_pflush_workaround(on);
                    }
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
                                    SerialBridgeStatus::Pty(path) => {
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
                        // A state can't hold a SCSI command in flight
                        self.config.scsi_settle();
                        if let Err(e) = self.save_state(&path, screenshot) {
                            self.user_error(&format!("Failed to save state: {:?}", e));
                        }
                    }
                    EmulatorCommand::SetDebugFramebuffers(v) => {
                        if let EmulatorConfig::Compact(c) = &mut self.config {
                            c.bus.video.debug_framebuffers = v;
                        }
                    }
                    EmulatorCommand::SetFloppyWeakBitsSeed(seed) => {
                        self.seed_weak_bits(seed);
                    }
                    EmulatorCommand::SetFloppyRpmAdjustment(bus, slot, adjustment) => {
                        let result = self
                            .floppy_in(&bus, &slot)
                            .and_then(|floppy| floppy.set_rpm_adjustment(adjustment));
                        if let Err(e) = result {
                            self.user_error(&format!("{}: {:#}", self.slot_label(&bus, &slot), e));
                        }
                    }
                    EmulatorCommand::EthernetSetLink(bus, slot, link) => {
                        if let Err(e) = self.ethernet_in(&bus, &slot)?.set_link(link) {
                            self.user_error(&format!("{}: {:#}", self.slot_label(&bus, &slot), e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EthernetStartCapture(bus, slot, filename) => {
                        match self.ethernet_in(&bus, &slot)?.start_capture(&filename) {
                            Ok(_) => {
                                self.user_success(&format!(
                                    "{} #{}: Started pcap capture to '{}'",
                                    bus,
                                    slot,
                                    filename.display()
                                ));
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "{} #{}: Failed to start capture: {}",
                                    bus, slot, e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EthernetStopCapture(bus, slot) => {
                        match self.ethernet_in(&bus, &slot)?.stop_capture() {
                            Some((filename, count)) => {
                                self.user_success(&format!(
                                    "{} #{}: Stopped capture, wrote {} packets to '{}'",
                                    bus,
                                    slot,
                                    count,
                                    filename.display()
                                ));
                            }
                            None => {
                                self.user_warning(&format!(
                                    "{} #{}: No capture was active",
                                    bus, slot
                                ));
                            }
                        }
                        self.status_update()?;
                    }

                    EmulatorCommand::ReplaceAudioProvider(audio_provider) => {
                        self.set_audio_provider(audio_provider)?;
                    }
                }
            }
        }

        if !self.memory_notifiers.is_empty() && self.run {
            self.memory_notifiers_tick()?;
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

                // Poll bridges for incoming data and status changes
                if let Some(ref mut bridge) = self.serial_bridges[ch_idx] {
                    // Propagate SCC state to LocalTalk bridge
                    if bridge.is_localtalk() {
                        let sdlc_addr = self.config.scc().sdlc_address(ch);
                        bridge.set_node_address(sdlc_addr);
                        bridge
                            .set_address_search_mode(self.config.scc().is_address_search_mode(ch));
                    }
                }

                // Check for TX data from SCC (byte-stream path)
                if self.config.scc().has_tx_data(ch) {
                    let tx_data = self.config.scc_mut().take_tx(ch);

                    // Route through bridge if active, otherwise send to frontend
                    if let Some(ref mut bridge) = self.serial_bridges[ch_idx] {
                        let replies = bridge.write_from_scc(&tx_data);
                        for reply in replies {
                            self.config.scc_mut().push_rx_frame(ch, reply);
                        }
                    } else {
                        self.event_sender
                            .send(EmulatorEvent::SccTransmitData(ch, tx_data))?;
                    }
                }

                if let Some(ref mut bridge) = self.serial_bridges[ch_idx] {
                    bridge.advance_time(self.config.scc().elapsed_seconds());
                    // Check for state changes (e.g., new TCP connection, LocalTalk status)
                    let has_data = bridge.poll();
                    if has_data {
                        // A LocalTalk bridge reports every frame
                        if !bridge.is_localtalk()
                            || crate::mac::localtalk_bridge::LocalTalkBridge::wire_debug()
                        {
                            log::debug!(
                                "sending SerialBridgeStatus for {:?}: {:?}",
                                ch,
                                bridge.status()
                            );
                        }
                        self.event_sender
                            .send(EmulatorEvent::SerialBridgeStatus(ch, Some(bridge.status())))?;
                    }

                    // Read incoming data from bridge and send to SCC. For LocalTalk,
                    // only feed data when the SCC receiver is ready, so control
                    // packets (CTS) are not lost while the Mac still has its
                    // receiver disabled.
                    if let Some(frame) = self.config.scc_mut().take_unread_frame(ch) {
                        bridge.return_frame(frame);
                    }
                    let rx_data =
                        if bridge.is_localtalk() && !self.config.scc().is_rx_ready_for_data(ch) {
                            Vec::new()
                        } else {
                            bridge.read_to_scc()
                        };
                    if !rx_data.is_empty() {
                        self.config.scc_mut().push_rx(ch, &rx_data);
                    }
                }
            }

            // Replay next step in recording if currently replaying
            if let Some((t, c)) = self.replay_input.front()
                && *t <= self.get_cycles()
            {
                self.command_sender.send(c.clone()).unwrap();
                self.replay_input.pop_front().unwrap();
            }

            // Batch 10000 steps for performance reasons
            let mut steps = 10000;
            while steps > 0 && self.run {
                match self.config.cpu_run_steps(steps) {
                    Ok((done, reset)) => {
                        steps -= done;
                        if let Err(e) = self.after_steps(reset) {
                            self.step_failed(&e);
                        }
                    }
                    Err(e) => self.step_failed(&e),
                }
                self.poll_localtalk_bridge();
            }

            // Honour any writeback requests raised during this tick batch
            // (e.g. by the SWIM controller turning a drive motor off).
            self.flush_pending_writebacks(false);

            let iie_in_front = self.iie_card().is_some_and(|card| card.in_front());
            if iie_in_front != self.iie_in_front {
                self.iie_in_front = iie_in_front;
                self.event_sender
                    .send(EmulatorEvent::IieInFront(iie_in_front))?;
            }
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        Ok(ticks)
    }
}

#[cfg(test)]
mod tests {
    use super::{InspectRun, read_memory_range, search_memory};

    /// Memory with holes at the addresses in `holes`, in runs of at most 16
    /// bytes, holding the low byte of the address in units of 16 bytes
    fn test_read(addr: u32, out: &mut [u8], holes: std::ops::Range<u32>) -> InspectRun {
        if holes.contains(&addr) {
            return InspectRun::Hole(out.len().min((holes.end - addr) as usize));
        }
        let mut n = out.len().min(16 - (addr % 16) as usize);
        if addr < holes.start {
            n = n.min((holes.start - addr) as usize);
        }
        for (i, b) in out[..n].iter_mut().enumerate() {
            *b = ((addr as usize + i) / 16) as u8;
        }
        InspectRun::Data(n)
    }

    #[test]
    fn read_range_with_a_hole() {
        let data = read_memory_range(14, 8, |at, out| test_read(at, out, 18..20));
        assert_eq!(
            data,
            [
                Some(0),
                Some(0),
                Some(1),
                Some(1),
                None,
                None,
                Some(1),
                Some(1)
            ]
        );
    }

    #[test]
    fn search_finds_across_runs() {
        let read = |at: u32, out: &mut [u8]| test_read(at, out, 0x100..0x110);
        // Across the runs of 16 bytes
        assert_eq!(search_memory(0, 0xFFF, &[0, 1], read), Some(15));
        // From a start past the first one
        assert_eq!(search_memory(16, 0xFFF, &[1, 2], read), Some(31));
        // Not across a hole: $0F then $11
        assert_eq!(search_memory(0, 0xFFF, &[0xF, 0x11], read), None);
        // Up to the last address only
        assert_eq!(search_memory(0, 0x1F, &[1, 2], read), None);
        assert_eq!(search_memory(0, 0x20, &[1, 2], read), Some(31));
        assert_eq!(search_memory(0, 0xFFF, &[], read), None);
    }

    #[test]
    fn search_stops_at_the_end_of_the_address_space() {
        let read = |at: u32, out: &mut [u8]| {
            InspectRun::Hole(
                out.len()
                    .min((u64::from(u32::MAX) - u64::from(at) + 1) as usize),
            )
        };
        assert_eq!(search_memory(0xFFFF_FF00, u32::MAX, &[1], read), None);
    }
}
