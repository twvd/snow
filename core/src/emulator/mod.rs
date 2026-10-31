pub mod comm;
pub mod devices;

#[cfg(feature = "savestates")]
pub mod save;

use serde::{Deserialize, Serialize};
use snow_floppy::loaders::{FloppyImageSaver, Moof};
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
#[cfg(feature = "savestates")]
use crate::emulator::save::{load_state_from, save_state_to};
use crate::keymap::KeyEvent;
use crate::lisa::bus::LisaBus;
use crate::mac::compact::bus::CompactMacBus;
use crate::mac::machine_bus::{MachineBus020, MachineBus040};
use crate::mac::macii::bus::MacIIBus;
use crate::mac::maclc::lc_475_bus::Lc475Bus;
use crate::mac::maclc::lc_i_bus::LcIBus;
use crate::mac::maclc::lc_iii_bus::LcIIIBus;
use crate::mac::maclc::pds_iie::IieCardSettings;
use crate::mac::portable::bus::MacPortableBus;
use crate::mac::quadra::bus::Quadra700Bus;
use crate::mac::quadra::q800_bus::Quadra800Bus;
use crate::mac::scc::Scc;
use crate::mac::scsi::target::ScsiTargetEvent;
use crate::mac::serial_bridge::SerialDevice;
use crate::mac::swim::drive::DriveType;
use crate::mac::{
    AbsoluteMouse, ExtraROMs, MacModel, MacMonitor, NubusCardConfig, NubusDeviceKind,
};
use crate::renderer::AudioProvider;
use crate::renderer::channel::ChannelRenderer;
use crate::renderer::{DisplayHandle, FrameReceiver, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::Byte;

use anyhow::{Result, bail};
use log::*;
use std::fmt;

use crate::cpu_m68k::regs::{Register, RegisterFile};
use crate::emulator::comm::{EmulatorSpeed, UserMessageType};
use crate::mac::rtc::Rtc;
use crate::mac::scsi::bus::ScsiBus;
use crate::mac::scsi::disk_image::DiskImage;
use crate::mac::swim::Swim;
use comm::{
    Breakpoint, EmulatorCommand, EmulatorCommandSender, EmulatorEvent, EmulatorEventReceiver,
    EmulatorStatus, GuestScreen, InputRecording, MemoryNotifierRange,
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
                        Self::Lisa(inner) => &inner.$($ref_target)*,
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
                        Self::Lisa(inner) => &mut inner.$($mut_ref_target)*,
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
                        Self::Lisa(inner) => inner.$($immut_call_target)*,
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
                        Self::Lisa(inner) => inner.$($mut_call_target)*,
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

/// MMU32Bit: true when the guest runs in 32-bit mode (Inside Macintosh V, p. V-592)
const ADDR_MMU32BIT: Address = 0x0CB2;
/// Color QuickDraw's list of graphics devices and the main one, handles to
/// GDevice records (Inside Macintosh: Imaging With QuickDraw, ch. 5)
const ADDR_DEVICELIST: Address = 0x08A8;
const ADDR_MAINDEVICE: Address = 0x08A4;
/// The Device Manager's unit table, of handles to the drivers' DCEs
const ADDR_UTABLEBASE: Address = 0x011C;
/// Offsets in a GDevice record: the driver's reference number, the flags, the
/// next device and the bounds (top, left, bottom, right)
const GD_REFNUM: Address = 0;
const GD_FLAGS: Address = 20;
const GD_NEXTGD: Address = 30;
const GD_RECT: Address = 34;
/// gdFlags: the device is a screen, and one the guest uses. Without a 'scrn'
/// resource, from the Monitors control panel, only the startup screen is used
/// (Inside Macintosh V, p. V-126).
const GD_SCREEN_DEVICE: u16 = 1 << 13;
const GD_SCREEN_ACTIVE: u16 = 1 << 15;
/// Offset of dCtlDevBase in a DCE: the base address of the device, which the
/// Slot Manager gives its driver (Inside Macintosh: Devices, ch. 1). The slot
/// is found from it rather than from dCtlSlot, which isn't the slot whose space
/// the device answers in for built-in video: on the Quadra 700, the driver of
/// the video at $F900_0000 has slot 0.
const DCTL_DEVBASE: Address = 42;
/// Most graphics devices looked at, against a broken list
const MAX_GDEVICES: usize = 16;
/// Time between two looks at where the guest puts the displays
const SCREENS_INTERVAL: Duration = Duration::from_secs(1);

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
    Compact(Box<CpuM68000<CompactMacBus<ChannelRenderer>>>),
    /// Macintosh Portable
    Portable(Box<CpuM68000<MacPortableBus<ChannelRenderer>>>),
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
    /// Apple Lisa
    Lisa(Box<CpuM68000<LisaBus<ChannelRenderer>>>),
}

dispatch! {
    immutable_refs {
        fn swim(&self) -> &Swim { bus.swim }
        fn scc(&self) -> &Scc { bus.scc }
        fn cpu_regs(&self) -> &RegisterFile { regs }
    }

    mutable_refs {
        fn swim_mut(&mut self) -> &mut Swim { bus.swim }
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
        fn display_names(&self) -> Vec<String> { bus.display_names() }
    }

    mutable_calls {
        fn scsi_mut(&mut self) -> &mut ScsiBus { bus.scsi.bus_mut() }
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
        fn cpu_set_pc(&mut self, pc: Address) -> Result<()> { set_pc(pc) }
        fn cpu_get_clr_breakpoint_hit(&mut self) -> bool { get_clr_breakpoint_hit() }
        fn cpu_read_history(&mut self) -> Option<&[HistoryEntry]> { read_history() }
        fn cpu_read_systrap_history(&mut self) -> Option<&[SystrapHistoryEntry]> { read_systrap_history() }
        fn cpu_run_steps(&mut self, steps: u32) -> Result<(u32, bool)> { run_steps(steps) }
        fn cpu_prefetch_refill(&mut self) -> Result<()> { prefetch_refill() }
        fn cpu_reset(&mut self) -> Result<()> { reset() }

        fn bus_reset(&mut self) -> Result<bool> { bus.reset(true) }
        fn bus_take_reset_request(&mut self) -> bool { bus.take_reset_request() }
        fn after_deserialize(&mut self, renderers: Vec<ChannelRenderer>) -> () { bus.after_deserialize(renderers) }
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

        fn rtc_mut(&mut self) -> &mut Rtc { bus.rtc_mut() }
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
    serial_bridges: [Option<Box<dyn SerialDevice>>; 2],
    /// What the machine remembers of its devices
    device_book: devices::DeviceBook,
    /// The machine's buses, as the frontend sees them. Built again when a device comes or goes.
    bus_list: Arc<Vec<devices::Bus>>,
    /// The status of every device, shared with the frontend
    status_block: Arc<devices::StatusBlock>,
    audio_provider: Option<Arc<Mutex<dyn AudioProvider>>>,
    /// Ranges of memory the frontend wants to be told about, by id
    memory_notifiers: BTreeMap<u32, MemoryNotifier>,
    /// Map state the watched ranges were set for
    memory_map_state: Option<MemoryMapState>,
    /// Last look at the memory notifiers
    memory_notify_time: Instant,
    /// The slot of each display, as the guest's drivers have it
    display_slots: Vec<u8>,
    /// Where the guest puts the displays, at the last look
    screens: Vec<GuestScreen>,
    screens_time: Instant,
}

/// The NuBus slot of a device's base address: $Fsxx_xxxx in its standard slot
/// space, $sxxx_xxxx in its super slot space, or $sx_xxxx in 24-bit mode
fn base_slot(base: Address) -> u8 {
    let slot = match base {
        0..=0xFF_FFFF => base >> 20,
        _ if base >> 28 == 0xF => base >> 24,
        _ => base >> 28,
    };
    (slot & 0xF) as u8
}

/// Creates one renderer per NuBus card that shows a screen, recording the frame receivers (in
/// the same order) so they can be paired with the display names afterwards.
fn make_renderers(
    nubus: &[NubusCardConfig<'_>],
    receivers: &mut Vec<FrameReceiver>,
) -> Result<Vec<ChannelRenderer>> {
    nubus
        .iter()
        .filter(|c| c.has_display())
        .map(|_| {
            let renderer = ChannelRenderer::new(0, 0)?;
            receivers.push(renderer.get_receiver());
            Ok(renderer)
        })
        .collect()
}

/// Pairs display names with their frame receivers into display handles. Both
/// lists must be in the same order (display install order).
fn build_displays(names: Vec<String>, receivers: Vec<FrameReceiver>) -> Vec<DisplayHandle> {
    debug_assert_eq!(names.len(), receivers.len());
    names
        .into_iter()
        .zip(receivers)
        .map(|(name, receiver)| DisplayHandle { name, receiver })
        .collect()
}

/// The cards of a Mac II in slot order: the ones given, else the card of the extra ROMs in
/// slot $9
fn macii_cards<'a>(
    extra_roms: &[ExtraROMs<'a>],
    video_cards: &[NubusCardConfig<'a>],
    monitor: Option<MacMonitor>,
) -> Result<Vec<NubusCardConfig<'a>>> {
    let mut cards = video_cards.to_vec();
    if cards.is_empty() {
        cards.extend(extra_roms.iter().find_map(|p| {
            let (kind, rom) = match p {
                ExtraROMs::MDC12(data) => (NubusDeviceKind::Mdc12, *data),
                ExtraROMs::Toby(data) => (NubusDeviceKind::Toby, *data),
                _ => return None,
            };
            Some(NubusCardConfig {
                slot: 0x9,
                kind,
                rom,
                monitor: Some(monitor.unwrap_or_default()),
            })
        }));
    }
    if cards.is_empty() {
        bail!("Macintosh II requires display card ROM")
    }
    cards.sort_by_key(|c| c.slot);
    Ok(cards)
}

impl Emulator {
    pub fn new(
        rom: &[u8],
        extra_roms: &[ExtraROMs],
        model: MacModel,
    ) -> Result<(Self, Vec<DisplayHandle>)> {
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
            &[],
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
        video_cards: &[NubusCardConfig],
    ) -> Result<(Self, Vec<DisplayHandle>)> {
        // Set up channels
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();

        // Frame receivers for each display output, collected in the same order
        // as the renderers are installed (i.e. the order display_names() reports).
        let mut receivers: Vec<FrameReceiver> = Vec::new();

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
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let bus = CompactMacBus::new(
                    model,
                    rom,
                    extension_rom,
                    renderer,
                    mouse_mode,
                    ram_size,
                    override_fdd_type,
                );
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
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let bus =
                    MacPortableBus::new(model, rom, extension_rom, renderer, mouse_mode, ram_size);
                let cpu = Box::new(CpuM68000::new(bus, FPU_NONE));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::Portable(cpu)
            }
            MacModel::MacII | MacModel::MacIIFDHD => {
                assert!(override_fdd_type.is_none());

                let nubus = macii_cards(extra_roms, video_cards, monitor)?;

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                let renderers = make_renderers(&nubus, &mut receivers)?;
                // Initialize bus and CPU. A 68851 takes the place of the AMU.
                let bus = MacIIBus::new(
                    model,
                    rom,
                    &nubus,
                    extension_rom,
                    renderers,
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

                let nubus = macii_cards(extra_roms, video_cards, monitor)?;

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let renderers = make_renderers(&nubus, &mut receivers)?;
                let bus = MacIIBus::new(
                    model,
                    rom,
                    &nubus,
                    extension_rom,
                    renderers,
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
                    monitor: None,
                }];

                // Find extension ROM if present
                let extension_rom = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::ExtensionROM(data) => Some(*data),
                    _ => None,
                });

                // Initialize bus and CPU
                let renderers = make_renderers(&nubus, &mut receivers)?;
                let bus = MacIIBus::new(
                    model,
                    rom,
                    &nubus,
                    extension_rom,
                    renderers,
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
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let mut bus = LcIBus::new(
                    model, rom, renderer, mouse_mode, ram_size, monitor, vram_size, iie_card,
                );
                bus.set_security_jumper(iie_card.is_some_and(|s| s.security_jumper));
                let fpu = if fpu_enabled { FPU_M68882 } else { FPU_NONE };
                EmulatorConfig::MacLC(Box::new(CpuM68020::new(MachineBus020::LcI(bus), fpu)))
            }
            MacModel::Lisa25 => {
                let Some(video_prom) = extra_roms.iter().find_map(|p| match p {
                    ExtraROMs::LisaVideoProm(data) => Some(*data),
                    _ => None,
                }) else {
                    bail!("Lisa video state PROM required");
                };
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let bus = LisaBus::new(model, rom, video_prom, renderer, mouse_mode, ram_size);
                EmulatorConfig::Lisa(Box::new(CpuM68000::new(bus, FPU_NONE)))
            }
            MacModel::LCII => {
                assert!(override_fdd_type.is_none());

                // Initialize bus and CPU
                // The FPU is an option on an expansion card
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let mut bus = LcIBus::new(
                    model, rom, renderer, mouse_mode, ram_size, monitor, vram_size, iie_card,
                );
                bus.set_security_jumper(iie_card.is_some_and(|s| s.security_jumper));
                let fpu = if fpu_enabled { FPU_M68882 } else { FPU_NONE };
                EmulatorConfig::MacLCII(Box::new(CpuM68030::new(MachineBus020::LcI(bus), fpu)))
            }
            MacModel::LCIII => {
                assert!(override_fdd_type.is_none());

                // Initialize bus and CPU
                // The FPU is an option on an expansion card
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let mut bus = LcIIIBus::new(
                    model, rom, renderer, mouse_mode, ram_size, monitor, vram_size, iie_card,
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

                // Initialize bus and CPU. The displays of the cards come after the
                // built-in one.
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let mut cards = video_cards.to_vec();
                cards.sort_by_key(|c| c.slot);
                let card_renderers = make_renderers(&cards, &mut receivers)?;
                let bus = Quadra700Bus::new(
                    model,
                    rom,
                    extension_rom,
                    renderer,
                    monitor.unwrap_or_default(),
                    &cards,
                    card_renderers,
                    mouse_mode,
                    ram_size,
                );
                let cpu = Box::new(CpuM68040::new(MachineBus040::Quadra700(bus), FPU_M68040));
                assert_eq!(cpu.get_type(), model.cpu_type());

                EmulatorConfig::Quadra700(cpu)
            }
            MacModel::LC475 => {
                assert!(override_fdd_type.is_none());

                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let bus = Lc475Bus::new(
                    model,
                    rom,
                    renderer,
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

                // The displays of the cards come after the built-in one
                let renderer = ChannelRenderer::new(0, 0)?;
                receivers.push(renderer.get_receiver());
                let mut cards = video_cards.to_vec();
                cards.sort_by_key(|c| c.slot);
                let card_renderers = make_renderers(&cards, &mut receivers)?;
                let bus = Quadra800Bus::new(
                    model,
                    rom,
                    renderer,
                    monitor.unwrap_or_default(),
                    &cards,
                    card_renderers,
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
        let mut device_book = devices::DeviceBook::default();
        if let Some(settings) = iie_card {
            device_book.set_iie_chain(&settings);
        }

        // Pair each display output's name (from the bus, in install order) with
        // its frame receiver (collected in the same order above).
        let displays = build_displays(config.display_names(), receivers);

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
            device_book,
            bus_list: Arc::default(),
            status_block: Arc::new(devices::StatusBlock::new()),
            audio_provider: None,
            memory_notifiers: BTreeMap::new(),
            memory_map_state: None,
            memory_notify_time: Instant::now(),
            display_slots: vec![],
            screens: vec![],
            screens_time: Instant::now(),
        };
        emu.display_slots = emu.read_display_slots();
        emu.rebuild_bus_list();
        emu.status_update()?;

        for ch in crate::mac::scc::SccCh::iter() {
            emu.event_sender
                .send(EmulatorEvent::SerialBridgeStatus(ch, None))?;
        }

        Ok((emu, displays))
    }

    /// Restores a saved emulator state into a new Emulator instance
    #[cfg(feature = "savestates")]
    pub fn load_state<P: AsRef<Path>, PT: AsRef<Path>>(
        path: P,
        tmpdir: PT,
    ) -> Result<(Self, Vec<DisplayHandle>)> {
        let (cmds, cmdr) = crossbeam_channel::unbounded();
        let (statuss, statusr) = crossbeam_channel::unbounded();
        let time = Instant::now();

        let fstr = path
            .as_ref()
            .file_name()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let f = File::open(path)?;

        let (mut config, devices, slots) = load_state_from(f, tmpdir)?;

        // Mint one renderer per display output the restored machine exposes,
        // keeping the receivers so the frontend can pick up the frames.
        let names = config.display_names();
        let mut receivers: Vec<FrameReceiver> = Vec::with_capacity(names.len());
        let mut renderers: Vec<ChannelRenderer> = Vec::with_capacity(names.len());
        for _ in &names {
            let renderer = ChannelRenderer::new(0, 0)?;
            receivers.push(renderer.get_receiver());
            renderers.push(renderer);
        }
        config.after_deserialize(renderers);
        let displays = build_displays(names, receivers);
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
            device_book: Default::default(),
            bus_list: Arc::default(),
            status_block: Arc::new(devices::StatusBlock::new()),
            audio_provider: None,
            memory_notifiers: BTreeMap::new(),
            memory_map_state: None,
            memory_notify_time: Instant::now(),
            display_slots: vec![],
            screens: vec![],
            screens_time: Instant::now(),
        };
        emu.display_slots = emu.read_display_slots();
        // The state doesn't hold where the devices came from, the list does
        let devices = serde_json::from_str(&devices).unwrap_or_else(|e| {
            log::warn!("No device list in the state file: {}", e);
            vec![]
        });
        emu.rebuild_bus_list();
        emu.restore_slots(slots)?;
        emu.reinstate_devices(devices);
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

        Ok((emu, displays))
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

    /// Whether the Apple IIe Card is in front, on a machine that has one
    fn iie_card_in_front(&mut self) -> bool {
        let card = match &mut self.config {
            EmulatorConfig::MacLC(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::MacLCII(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::MacLCIII(inner) => inner.bus.iie_card_mut(),
            EmulatorConfig::LC475(inner) => inner.bus.iie_card_mut(),
            _ => None,
        };
        card.is_some_and(|card| card.in_front())
    }

    fn status_update(&mut self) -> Result<()> {
        for i in 0..self.config.swim().drives.len() {
            self.writeback_ejected(i);
        }
        for i in 0..self.config.swim().drives.len() {
            if let Some(img) = self.config.swim_mut().drives[i].take_ejected_image()
                && let Some(addr) = self.find_slot(devices::bus_id::SWIM, &(i + 1).to_string())
            {
                self.event_sender
                    .send(EmulatorEvent::FloppyEjected(addr, img))?;
            }
        }
        let scsi_events: Vec<_> = self
            .config
            .scsi_mut()
            .targets
            .iter_mut()
            .enumerate()
            .filter_map(|(i, t)| Some((i, t.as_mut()?.take_event()?)))
            .collect();
        for (id, event) in scsi_events {
            match event {
                ScsiTargetEvent::MediaEjected => {
                    if let Some(addr) = self.find_slot(devices::bus_id::SCSI, &id.to_string()) {
                        self.event_sender
                            .send(EmulatorEvent::MediaEjected(addr))
                            .unwrap();
                    }
                }
                #[cfg(feature = "printer")]
                ScsiTargetEvent::PageSaved(f) => {
                    self.event_sender
                        .send(EmulatorEvent::UserMessage(
                            UserMessageType::Notice,
                            format!("LaserWriter: page saved as '{}'", f),
                        ))
                        .unwrap();
                }
            }
        }

        self.publish_status();

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

    /// The slot of each display, as the guest's drivers have it, on the
    /// machines that can have more than one
    fn read_display_slots(&self) -> Vec<u8> {
        match &self.config {
            EmulatorConfig::MacII(inner) => inner.bus.macii().display_slots(),
            EmulatorConfig::MacII30(inner) => inner.bus.macii().display_slots(),
            EmulatorConfig::Quadra700(inner) => inner.bus.quadra700().display_slots(),
            EmulatorConfig::Quadra800(inner) => inner.bus.quadra800().display_slots(),
            _ => vec![],
        }
    }

    fn inspect_long(&mut self, addr: Address) -> Option<u32> {
        let high = self.config.cpu_inspect_read_word(addr, false)?;
        let low = self.config.cpu_inspect_read_word(addr + 2, false)?;
        Some((u32::from(high) << 16) | u32::from(low))
    }

    /// A pointer in guest memory, as the guest follows it. In 24-bit mode the
    /// top byte isn't part of the address, and master pointers keep the flags
    /// of their handle in it: StripAddress (Inside Macintosh V, p. V-593).
    fn inspect_pointer(&mut self, addr: Address, mode32: bool) -> Option<Address> {
        let pointer = self.inspect_long(addr)?;
        Some(if mode32 {
            pointer
        } else {
            pointer & 0x00FF_FFFF
        })
    }

    /// Where the guest puts the displays: the screens of Color QuickDraw's
    /// device list, each found by the slot of its driver. None if the list
    /// can't be read.
    fn read_screens(&mut self) -> Option<Vec<GuestScreen>> {
        let mut screens = vec![];
        let mode32 = self.config.cpu_inspect_read(ADDR_MMU32BIT, false)? != 0;
        let main = self.inspect_pointer(ADDR_MAINDEVICE, mode32)?;
        let unit_table = self.inspect_pointer(ADDR_UTABLEBASE, mode32)?;
        let mut handle = self.inspect_pointer(ADDR_DEVICELIST, mode32)?;
        for _ in 0..MAX_GDEVICES {
            if handle == 0 {
                break;
            }
            let gd = self.inspect_pointer(handle, mode32)?;
            let flags = self.config.cpu_inspect_read_word(gd + GD_FLAGS, false)?;
            let refnum = self.config.cpu_inspect_read_word(gd + GD_REFNUM, false)? as i16;
            if flags & GD_SCREEN_DEVICE != 0 && refnum < 0 {
                let unit = (-i32::from(refnum) - 1) as Address;
                let dce_handle = self.inspect_pointer(unit_table + 4 * unit, mode32)?;
                let dce = self.inspect_pointer(dce_handle, mode32)?;
                let slot = base_slot(self.inspect_long(dce + DCTL_DEVBASE)?);
                let mut rect = [0; 4];
                for (i, v) in rect.iter_mut().enumerate() {
                    *v = self
                        .config
                        .cpu_inspect_read_word(gd + GD_RECT + 2 * i as Address, false)?
                        as i16;
                }
                let [top, left, bottom, right] = rect;
                if let Some(display) = self.display_slots.iter().position(|&s| s == slot)
                    && right > left
                    && bottom > top
                {
                    screens.push(GuestScreen {
                        display,
                        left,
                        top,
                        right,
                        bottom,
                        main: handle == main,
                        active: flags & GD_SCREEN_ACTIVE != 0,
                    });
                }
            }
            handle = self.inspect_pointer(gd + GD_NEXTGD, mode32)?;
        }
        Some(screens)
    }

    /// The guest's screens are only looked at with several displays and the
    /// absolute mouse: with the relative mouse, the guest may not be Mac OS
    /// (A/UX, Linux) and its memory is left alone.
    fn screens_wanted(&self) -> bool {
        let mode = match &self.config {
            EmulatorConfig::MacII(inner) => inner.bus.macii().mouse_mode(),
            EmulatorConfig::MacII30(inner) => inner.bus.macii().mouse_mode(),
            EmulatorConfig::Quadra700(inner) => inner.bus.quadra700().mouse_mode(),
            EmulatorConfig::Quadra800(inner) => inner.bus.quadra800().mouse_mode(),
            _ => return false,
        };
        self.display_slots.len() > 1 && mode == MouseMode::Absolute
    }

    /// Looks where the guest puts the displays, when there are several, and
    /// tells the frontend when that changes
    fn screens_tick(&mut self) -> Result<()> {
        if !self.screens_wanted() {
            if !self.screens.is_empty() {
                self.screens.clear();
                self.event_sender.send(EmulatorEvent::Screens(vec![]))?;
            }
            return Ok(());
        }
        if self.screens_time.elapsed() < SCREENS_INTERVAL {
            return Ok(());
        }
        self.screens_time = Instant::now();
        if let Some(screens) = self.read_screens()
            && screens != self.screens
        {
            log::info!("Screens: {:?}", screens);
            self.screens.clone_from(&screens);
            self.event_sender.send(EmulatorEvent::Screens(screens))?;
        }
        Ok(())
    }

    /// Reports a new position of the mouse in absolute mode to the OS, in its
    /// low memory globals, from a position on one of the displays. If they
    /// have no translation at the moment, or the guest doesn't use that
    /// display, this position is skipped.
    fn mouse_update_abs(&mut self, display: usize, x: u16, y: u16) {
        // With several displays, a display's pixels are where the guest puts it
        let (x, y) = if self.display_slots.len() > 1 {
            let Some(screen) = self
                .screens
                .iter()
                .find(|s| s.display == display && s.active)
            else {
                return;
            };
            (
                x.wrapping_add_signed(screen.left),
                y.wrapping_add_signed(screen.top),
            )
        } else {
            (x, y)
        };
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

    pub fn set_audio_provider(&mut self, provider: Arc<Mutex<dyn AudioProvider>>) -> Result<()> {
        self.audio_provider = Some(provider);
        self.config
            .set_audio_provider(&mut *self.audio_provider.as_ref().unwrap().lock().unwrap())
    }

    pub fn attach_disk_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        self.config.scsi_mut().attach_disk_image_at(image, scsi_id)
    }

    pub fn insert_cdrom_image_at(
        &mut self,
        image: Box<dyn DiskImage>,
        scsi_id: usize,
    ) -> Result<()> {
        self.config.scsi_mut().insert_cdrom_image_at(image, scsi_id)
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

    /// Saves the floppy in `drive` back to its source file using the writer
    /// matching its source format. No-op if writeback is not currently armed
    /// for the drive. Clears the dirty + pending flags on success.
    fn try_writeback(&mut self, drive: usize) {
        let drv = &self.config.swim().drives[drive];
        if !drv.writeback_enabled || !drv.floppy.is_dirty() {
            return;
        }
        let Some(path) = drv.image_path.clone() else {
            return;
        };

        let result = crate::util::atomic_write(&path, |f| {
            snow_floppy::loaders::save_image(&self.config.swim().drives[drive].floppy, f)
        });

        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        match result {
            Ok(()) => {
                let drv = &mut self.config.swim_mut().drives[drive];
                drv.floppy.clear_dirty();
                drv.pending_writeback = false;
                log::info!(
                    "Floppy #{}: writeback auto-saved '{}'",
                    drive + 1,
                    display_name
                );
            }
            Err(e) => {
                self.config.swim_mut().drives[drive].pending_writeback = false;
                self.user_error(&format!(
                    "Floppy #{}: writeback to '{}' failed: {:#}",
                    drive + 1,
                    display_name,
                    e
                ));
            }
        }
    }

    /// Saves a disk ejected by the guest with unsaved writes back to its
    /// source file. No-op unless the eject armed it.
    fn writeback_ejected(&mut self, drive: usize) {
        let drv = &mut self.config.swim_mut().drives[drive];
        let Some(path) = drv.eject_writeback.take() else {
            return;
        };
        let Some(img) = drv.floppy_ejected.as_deref() else {
            return;
        };

        let result = crate::util::atomic_write(&path, |f| snow_floppy::loaders::save_image(img, f));
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        match result {
            Ok(()) => log::info!(
                "Floppy #{}: writeback saved ejected disk '{}'",
                drive + 1,
                display_name
            ),
            Err(e) => self.user_error(&format!(
                "Floppy #{}: writeback of ejected disk to '{}' failed: {:#}",
                drive + 1,
                display_name,
                e
            )),
        }
    }

    /// Drains queued writeback requests across all drives. With `force`,
    /// ignores [`FloppyDrive::pending_writeback`] and saves any dirty drive
    /// that has writeback armed.
    fn flush_pending_writebacks(&mut self, force: bool) {
        for i in 0..self.config.swim().drives.len() {
            self.writeback_ejected(i);
            let drv = &self.config.swim().drives[i];
            if drv.writeback_enabled && (force || drv.pending_writeback) && drv.floppy.is_dirty() {
                self.try_writeback(i);
            }
        }
    }

    fn poll_localtalk_bridge(&mut self) {
        // Demand-driven LocalTalk polling: poll immediately when Mac re-enables RX
        // Only poll if RX is enabled and no character is available (ready for new packet)
        let ch = crate::mac::scc::SccCh::B;
        if self.config.scc_mut().take_localtalk_poll_needed(ch)
            && self.config.scc().is_rx_ready_for_data(ch)
            && let Some(device) = self.serial_bridges[ch as usize].as_mut()
        {
            let scc = self.config.scc();
            device.set_station(scc.sdlc_address(ch), scc.is_address_search_mode(ch));
            let frames = self.config.scc_mut().take_tx_frames(ch);
            if !frames.is_empty() {
                for frame in &frames {
                    device.tx_frame(frame);
                }
                // The same bytes are in the byte queue too
                self.config.scc_mut().take_tx(ch);
            }
            if self.config.scc().has_tx_data(ch) {
                let tx_data = self.config.scc_mut().take_tx(ch);
                device.tx_bytes(&tx_data);
            }

            // Now poll for incoming data and pending CTS
            device.poll();
            let rx_data = device.rx();
            if !rx_data.is_empty() {
                self.config.scc_mut().push_rx(ch, &rx_data);
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

    #[allow(clippy::significant_drop_tightening)]
    #[cfg(feature = "savestates")]
    fn save_state(&mut self, p: &Path, screenshot: Option<Vec<u8>>) -> Result<()> {
        let devices = serde_json::to_string(&self.devices())?;
        let slots = self.slot_states()?;
        let mut f = File::create(p)?;
        let time = Instant::now();

        save_state_to(&f, &self.config, screenshot, devices, &slots)?;

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
                    EmulatorCommand::MouseUpdateAbsolute { x, y, display } => {
                        if let Some(r) = self.record_input.as_mut() {
                            r.push((cycles, cmd));
                        }

                        self.mouse_update_abs(display, x, y);
                    }
                    EmulatorCommand::SetMouseMode(mode) => {
                        log::info!("Mouse mode: {:?}", mode);
                        self.config.set_mouse_mode(mode);
                    }
                    EmulatorCommand::AddDevice(config) => {
                        if let Err(e) = self.add_device(&config) {
                            self.user_warning(&format!("{}: {:#}", config.label(), e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::RemoveDevice(addr) => {
                        if let Err(e) = self.remove_device(addr) {
                            self.user_warning(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::Quit => {
                        info!("Emulator terminating");
                        self.flush_pending_writebacks(true);
                        self.config.video_blank()?;
                        return Ok(0);
                    }
                    EmulatorCommand::SetIieGamePort(port) => {
                        let card = self.iie_card_mut();
                        if let Some(card) = card {
                            card.set_game_port(port);
                        }
                    }
                    EmulatorCommand::InsertFloppyImage(addr, mut img, wp) => {
                        let Ok(drive) = self.drive_at(addr) else {
                            continue;
                        };
                        if wp {
                            img.set_force_wp();
                        }
                        if let Err(e) = self.config.swim_mut().disk_insert(drive, *img) {
                            self.user_error(&format!("Cannot insert disk: {}", e));
                        } else {
                            let drv = &mut self.config.swim_mut().drives[drive];
                            drv.media_path = None;
                            drv.media_read_only = false;
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::SetFloppyWriteback(addr, enabled) => {
                        if let Err(e) = self.set_floppy_writeback(addr, enabled) {
                            self.user_error(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::BranchDisk(addr, filename) => {
                        if let Err(e) = self.branch_disk(addr, &filename) {
                            self.user_error(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::LoadMedia(addr, filename, wp) => {
                        if let Err(e) = self.load_media(addr, &filename, wp) {
                            self.user_error(&format!(
                                "Cannot load '{}': {:#}",
                                filename.display(),
                                e
                            ));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::EjectMedia(addr) => {
                        if let Err(e) = self.eject_media(addr) {
                            self.user_error(&format!("{:#}", e));
                        }
                        self.status_update()?;
                    }
                    EmulatorCommand::SaveFloppy(addr, filename) => {
                        let Ok(drive) = self.drive_at(addr) else {
                            continue;
                        };
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
                    #[cfg(feature = "savestates")]
                    EmulatorCommand::SaveState(path, screenshot) => {
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
                        self.config.swim_mut().seed_weak_bits(seed);
                    }
                    EmulatorCommand::SetFloppyRpmAdjustment(addr, adjustment) => {
                        let Ok(drive) = self.drive_at(addr) else {
                            continue;
                        };
                        if drive < self.config.swim_mut().drives.len() {
                            let drv = &mut self.config.swim_mut().drives[drive];
                            drv.rpm_adjustment = adjustment;
                            drv.update_ticks_per_bit();
                        }
                    }
                    #[cfg(feature = "ethernet")]
                    EmulatorCommand::EthernetSetLink(addr, link) => {
                        self.ethernet_at(addr, |ethernet| ethernet.set_link(link))??;
                    }
                    #[cfg(feature = "ethernet")]
                    EmulatorCommand::EthernetStartCapture(addr, filename) => {
                        let Ok(idx) = self.scsi_id_at(addr) else {
                            continue;
                        };
                        match self
                            .ethernet_at(addr, |ethernet| ethernet.start_capture(&filename))?
                        {
                            Ok(_) => {
                                self.user_success(&format!(
                                    "SCSI #{}: Started pcap capture to '{}'",
                                    idx,
                                    filename.display()
                                ));
                            }
                            Err(e) => {
                                self.user_error(&format!(
                                    "SCSI #{}: Failed to start capture: {}",
                                    idx, e
                                ));
                            }
                        }
                        self.status_update()?;
                    }
                    #[cfg(feature = "ethernet")]
                    EmulatorCommand::EthernetStopCapture(addr) => {
                        let Ok(idx) = self.scsi_id_at(addr) else {
                            continue;
                        };
                        match self.ethernet_at(addr, |ethernet| ethernet.stop_capture())? {
                            Some((filename, count)) => {
                                self.user_success(&format!(
                                    "SCSI #{}: Stopped capture, wrote {} packets to '{}'",
                                    idx,
                                    count,
                                    filename.display()
                                ));
                            }
                            None => {
                                self.user_warning(&format!("SCSI #{}: No capture was active", idx));
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
            self.screens_tick()?;
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

                // The SCC's station for a device carrying frames
                if let Some(device) = self.serial_bridges[ch_idx].as_mut() {
                    let scc = self.config.scc();
                    device.set_station(scc.sdlc_address(ch), scc.is_address_search_mode(ch));
                    let frames = self.config.scc_mut().take_tx_frames(ch);
                    if !frames.is_empty() {
                        for frame in &frames {
                            device.tx_frame(frame);
                        }
                        // The same bytes are in the byte queue too
                        self.config.scc_mut().take_tx(ch);
                    }
                }

                // Check for TX data from SCC
                if self.config.scc().has_tx_data(ch) {
                    let tx_data = self.config.scc_mut().take_tx(ch);

                    // Route through the device if there is one, otherwise send to frontend
                    if let Some(device) = self.serial_bridges[ch_idx].as_mut() {
                        device.tx_bytes(&tx_data);
                    } else {
                        self.event_sender
                            .send(EmulatorEvent::SccTransmitData(ch, tx_data))?;
                    }
                }

                if let Some(device) = self.serial_bridges[ch_idx].as_mut() {
                    // Check for state changes (e.g., new TCP connection, LocalTalk status)
                    if device.poll() {
                        self.event_sender
                            .send(EmulatorEvent::SerialBridgeStatus(ch, Some(device.status())))?;
                    }

                    // Read incoming data from the device and send to SCC
                    let rx_data = device.rx();
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

            let iie_in_front = self.iie_card_in_front();
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
