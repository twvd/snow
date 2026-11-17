use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::bus::access::{Bus68040, BusAccess, BusResponse, bus_access_long, bus_access_word_lane};
use crate::bus::native_clock::{C16M_FREQUENCY, C16M_PER_VIA, DeviceCycle, NativeClock};
use crate::bus::{
    Address, Bus, BusMember, InspectRun, InspectableBus, IrqSource, inspect_read_bytes,
};
use crate::debuggable::Debuggable;
use crate::devices::{
    BusOps, DeviceConfig, DeviceModel, DeviceOps, built_in, built_in_display, bus_id,
};
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MachineConfig, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::dma::{self, DmaOps, DmaOutcome, DmaRam, NubusDma};
#[cfg(feature = "ethernet")]
use crate::mac::ethernet::built_in::BuiltInSonic;
#[cfg(feature = "ethernet")]
use crate::mac::ethernet::{low_word_written, mac_prom_byte};
use crate::mac::macii::via2::Via2;
use crate::mac::nubus::Nubus;
use crate::mac::quadra::dafb::Dafb;
use crate::mac::rtc::Rtc;
use crate::mac::scc::{Scc, SccCh};
use crate::mac::scsi::esp::{Esp, esp_check};
use crate::mac::sound::easc::Easc;
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::via_common::{LineState, ViaLine};
use crate::mac::{AbsoluteMouse, MacModel, WARM_START_SIGNATURE};
use crate::renderer::{AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, null_audio_sink};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

/// Frequency of the bus of the real hardware, without overclocking: the
/// 25 MHz of the 68040
const BUS_FREQUENCY: Ticks = 25_000_000;

/// Wait states, in C16M ticks, of an access to a device on the I/O bus, on
/// top of the bus cycle itself. The I/O bus runs at 15.6672 MHz, "completely
/// asynchronous to the system bus clock", and the Relayer converts the
/// timing signals between the two buses (Quadra 700 Developer Note, "I/O bus
/// adapter ICs"): it synchronises the cycle with the I/O bus clock, and its
/// termination back with the 25 MHz bus. Not measured, a guess: the floppy
/// driver of the ROM fails to read disks without wait states, and works from
/// 2 on.
const IO_BUS_WAITS: Ticks = 6;

/// Extra wait states, in C16M ticks, of an SCC access, for PCLK. PCLK is
/// 8.336 MHz, asynchronous to C16M: ideally NativeClock would produce it too,
/// to synchronise the access with it, as device16_sync() does with C16M. For
/// now the access is stretched by about two PCLK cycles. Not measured.
const SCC_PCLK_WAITS: Ticks = 4;

/// Wait states of a NuBus transaction, approximate: two cycles of the 10 MHz
/// NuBus and the crossing between its clock and the 25 MHz bus add up to about
/// 7 bus clocks. The 68040's own bus cycle is two of them, so these are the
/// other five. The YANCC takes a write in a buffer and lets the CPU go on
/// before the transaction is done, which isn't modelled.
const NUBUS_WAITS: Ticks = 5;

/// Clock of the RTxCA and RTxCB pins of the SCC, 3.6864 MHz, which the baud
/// rate generators run from
const SCC_RTXC_FREQUENCY: Ticks = 3_686_400;

/// Silicon revision of the SONIC. Nobody has read it on a Quadra 700, so this is a guess: Linux
/// boot logs show revision 4 on the Quadra 650's built-in SONIC and on other Macs, the DP83932C
/// datasheet (and MAME) give 6 for the DP83932CVF, and QEMU's q800 uses 4.
#[cfg(feature = "ethernet")]
const SONIC_REVISION: u16 = 4;

/// What answers in a 16MB block of the address space
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    None,
    Ram,
    /// ROM, also in place of the RAM during the boot overlay
    Rom,
    /// I/O devices, repeating every 256KB
    Io,
    /// Test software region / extension ROM
    ExtRom,
    /// NuBus super slots
    SuperSlot,
    /// Built-in video (DAFB), slot $9
    Video,
    /// NuBus standard slots
    Slot,
}

/// The machine's RAM, as a device that masters the bus reaches it. It borrows only the fields
/// the DMA operations need, so that the bus can lend it while it ticks its cards.
struct Dma<'a> {
    ram: &'a mut [u8],
    overlay: bool,
    writes: &'a mut u32,
}

impl DmaRam for Dma<'_> {
    fn ram(&mut self) -> &mut [u8] {
        self.ram
    }

    fn ram_offset(&self, addr: Address) -> Option<usize> {
        let offset = addr as usize;
        (!self.overlay && offset < self.ram.len()).then_some(offset)
    }

    /// Those of a single cycle, as `Quadra700Bus::memory_waits()` gives them
    fn ram_timing(&self, _write: bool) -> (u32, Ticks) {
        let (waits, _) = Quadra700Bus::memory_waits(Region::Ram, 0).unwrap_or_default();
        (4, waits)
    }
}

impl DmaOps for Dma<'_> {
    fn read(&mut self, addr: Address, data: &mut [u8]) -> DmaOutcome {
        dma::read_ram(self, addr, data)
    }

    fn write(&mut self, addr: Address, data: &[u8]) -> DmaOutcome {
        // Counted even when it stops short: what was written until then is in the RAM
        *self.writes = self.writes.wrapping_add(1);
        dma::write_ram(self, addr, data)
    }
}

const fn region(overlay: bool, block: usize) -> Region {
    match block {
        0x00..=0x3F if overlay => Region::Rom,
        0x00..=0x3F => Region::Ram,
        0x40..=0x4F => Region::Rom,
        0x50..=0x51 => Region::Io,
        0x58..=0x5F => Region::ExtRom,
        0x60..=0xEF => Region::SuperSlot,
        0xF9 => Region::Video,
        0xF1..=0xFF => Region::Slot,
        _ => Region::None,
    }
}

/// Region of each 16MB block, with the boot overlay off and on
const MAP: [[Region; 256]; 2] = {
    let mut map = [[Region::None; 256]; 2];
    let mut block = 0;
    while block < 256 {
        map[0][block] = region(false, block);
        map[1][block] = region(true, block);
        block += 1;
    }
    map
};

#[derive(Serialize, Deserialize)]
pub struct Quadra700Bus {
    /// Interrupt level, until a tick of the bus or an access to the I/O area
    #[serde(skip)]
    irq: Option<Option<u8>>,

    /// The currently emulated Macintosh model
    model: MacModel,

    rom: Vec<u8>,
    extension_rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    pub(crate) via1: Via,
    pub(crate) via2: Via2,
    pub(crate) scc: Scc,
    pub(crate) asc: Easc,
    /// Interrupt PAL in the A/UX mode, for logging
    #[serde(skip)]
    aux_irq_mode: bool,
    /// Software interrupt asserted, for logging
    #[serde(skip)]
    soft_irq: bool,
    asc_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: Esp,
    /// A pseudo-DMA access is held off, waiting for DREQ
    scsi_dma_stalled: bool,

    rom_mask: usize,

    overlay: bool,
    amu_active: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Counts clock ticks at native frequency, regardless of overclocking
    native_clock: NativeClock,

    /// Last vblank time (for syncing to video)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for in Video speed mode.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,
    vblank_clock: Ticks,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Built-in video controller
    pub(crate) dafb: Dafb,

    /// Built-in Ethernet controller
    #[cfg(feature = "ethernet")]
    ethernet: BuiltInSonic,

    /// The cards in the NuBus slots $D and $E
    nubus: Nubus,
    /// Writes to the RAM by a device that masters the bus, counted for the views of the memory
    dma_writes: u32,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl Quadra700Bus {
    /// The devices on the logic board, by the id of their slot on the `Built-in` bus
    pub const BUILT_IN: &[(&str, DeviceModel)] = &[
        (built_in::VIDEO, DeviceModel::DafbVideo),
        #[cfg(feature = "ethernet")]
        (built_in::ETHERNET, DeviceModel::BuiltInSonic),
    ];

    /// Value to return on open bus
    /// Certain applications (e.g. Animation Toolkit) rely on this.
    const OPENBUS: u8 = 0;

    const IO_MASK: Address = 0x3_FFFF;

    /// CPU cycles before a pseudo-DMA access waiting for DREQ ends with a bus
    /// error. The length of this timeout is not documented.
    const SCSI_DMA_TIMEOUT: Ticks = 640;

    #[allow(clippy::too_many_arguments)]
    /// The logic board of a machine, with its built-in devices and nothing in its slots.
    /// `devices` is the list of device configs of the machine: it has what was chosen for a
    /// built-in device.
    pub fn new(machine: &MachineConfig, devices: &[DeviceConfig]) -> Result<Self> {
        let &MachineConfig {
            model,
            rom,
            extension_rom,
            ram_size,
            mouse_mode,
            ..
        } = machine;
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());

        if extension_rom.is_some() {
            log::info!("Extension ROM present");
        }

        let mut bus = Self {
            irq: None,
            model,

            rom: Vec::from(rom),
            extension_rom: extension_rom.map(Vec::from).unwrap_or_default(),
            ram: vec![0; ram_size],
            via1: Via::new(model),
            via2: Self::new_via2(model),
            aux_irq_mode: false,
            soft_irq: false,
            scc: Scc::new(SCC_RTXC_FREQUENCY),
            swim: Swim::new(model.fdd_hd(), C16M_FREQUENCY),
            // MODE pins strapped for a 16-bit DMA bus (schematic 050-8020)
            scsi: Esp::new(true),
            scsi_dma_stalled: false,
            asc: Easc::default(),
            asc_clock: 0,
            mouse_ready: false,

            rom_mask: rom.len() - 1,

            overlay: true,
            amu_active: false,
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(BUS_FREQUENCY),
            //last_audiosample: 0,
            vblank_time: Instant::now(),
            vblank_clock: 0,
            //vpa_sync: false,
            progkey_pressed: LatchingEvent::default(),

            dafb: Dafb::with_config(DeviceModel::DafbVideo, &built_in_display(devices)),
            #[cfg(feature = "ethernet")]
            ethernet: BuiltInSonic::new(SONIC_REVISION, devices)?,
            nubus: Nubus::new(BUS_FREQUENCY, model.nubus_slots()),
            dma_writes: 0,
            mouse_mode,
        };

        // Initialize ADB devices
        bus.via1.adb.add_device(AdbMouse::new());
        bus.via1.adb.add_device(AdbKeyboard::new());

        bus.disable_memtest();

        Ok(bus)
    }

    /// Makes the ROM skip its memory test
    ///
    /// This ROM does not look for the warm start signature at $CFC but in the
    /// long just below the memory table that it builds at the top of the RAM.
    /// The offset was measured by running the ROM with every RAM size.
    fn disable_memtest(&mut self) {
        let index = self.ram.len() - 0x30;
        info!("Skipping memory test");
        self.ram[index..index + 4].copy_from_slice(&WARM_START_SIGNATURE.1.to_be_bytes());
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self) {
        self.dafb.render().unwrap();

        self.asc.after_deserialize();
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.via1.rtc.effective_speed()
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        let sink = provider
            .create_stream(
                self.asc.sample_rate() as i32,
                AUDIO_CHANNELS as u8,
                AUDIO_BUFFER_SAMPLES as u16,
            )
            .unwrap_or_else(|_| null_audio_sink());
        self.asc.set_sink(sink);

        Ok(())
    }

    /// Wraps a device read result in a mapped region: logs a warning and falls
    /// back to open bus when the device did not handle the address. The
    /// enclosing region is considered mapped, so this never triggers a bus
    /// error.
    fn dev_read(addr: Address, val: Option<Byte>) -> Byte {
        val.unwrap_or_else(|| {
            warn!("Read from unimplemented address: {:08X}", addr);
            Self::OPENBUS
        })
    }

    /// Wraps a device write result in a mapped region: logs a warning if the
    /// device did not handle the address. The enclosing region is considered
    /// mapped, so this never triggers a bus error.
    fn dev_write(addr: Address, val: Byte, written: Option<()>) -> Option<()> {
        if written.is_none() {
            warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
        }
        Some(())
    }

    // RAM and ROM transfers stay within an aligned long word, which is in
    // one RAM page: when it is all there, it is one access

    fn ram_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let idx = addr as usize;
        if let Some(bytes) = self.ram.get(idx..idx + len as usize) {
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)));
        }
        // Ignore silently to avoid a lot of spam during memory tests
        Some((addr..addr + len).fold(0, |v, a| {
            let b = self.ram.get(a as usize).copied().unwrap_or(Self::OPENBUS);
            (v << 8) | u32::from(b)
        }))
    }

    fn ram_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        let idx = addr as usize;
        if let Some(ram) = self.ram.get_mut(idx..idx + len as usize) {
            ram.copy_from_slice(&val.to_be_bytes()[4 - len as usize..]);
            return Some(());
        }
        for a in addr..addr + len {
            let idx = a as usize;
            // Ignore silently to avoid a lot of spam during memory tests
            if idx < self.ram.len() {
                self.ram[idx] = (val >> (8 * (addr + len - 1 - a))) as Byte;
            }
        }
        Some(())
    }

    fn rom_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let idx = addr as usize & self.rom_mask;
        if let Some(bytes) = self.rom.get(idx..idx + len as usize) {
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)));
        }
        Some((addr..addr + len).fold(0, |v, a| {
            let idx = a as usize & self.rom_mask;
            (v << 8) | u32::from(self.rom.get(idx).copied().unwrap_or(Self::OPENBUS))
        }))
    }

    fn ext_rom_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        Some((addr..addr + len).fold(0, |v, a| {
            let rom = self.extension_rom.get((a - 0x5800_0000) as usize);
            (v << 8) | u32::from(rom.copied().unwrap_or(Self::OPENBUS))
        }))
    }

    fn ignore_write(&mut self, _addr: Address, _len: u32, _val: u32) -> Option<()> {
        Some(())
    }

    fn no_write(&mut self, _addr: Address, _len: u32, _val: u32) -> Option<()> {
        None
    }

    /// Unimplemented but some stray reads/writes end up here
    fn superslot_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        log::warn!("Unimplemented read from NuBus super slot: {:08X}", addr);
        Some(0)
    }

    fn superslot_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        log::warn!(
            "Unimplemented write to NuBus super slot: {:08X} {:08X}",
            addr,
            val
        );
        Some(())
    }

    /// The DAFB is reached a byte at a time
    fn video_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        if addr & 0xFF_FFFC == 0x80_0024 {
            self.dafb.scsi_drq = self.scsi.get_drq();
        }
        (addr..addr + len).try_fold(0, |v, a| {
            Some((v << 8) | u32::from(self.dafb.read(a & 0xFF_FFFF)?))
        })
    }

    fn video_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        for a in addr..addr + len {
            let val = (val >> (8 * (addr + len - 1 - a))) as Byte;
            self.dafb.write(a & 0xFF_FFFF, val)?;
        }
        Some(())
    }

    /// The I/O devices are reached a byte at a time
    fn io_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        (addr..addr + len).try_fold(0, |v, a| Some((v << 8) | u32::from(self.io_read_byte(a)?)))
    }

    fn io_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        for a in addr..addr + len {
            self.io_write_byte(a, (val >> (8 * (addr + len - 1 - a))) as Byte)?;
        }
        Some(())
    }

    fn io_read_byte(&mut self, addr: Address) -> Option<Byte> {
        match addr & Self::IO_MASK {
            // VIA 1
            0x0000_0000..=0x0000_1FFF => Some(Self::dev_read(addr, self.via1.read(addr))),
            // VIA 2
            0x0000_2000..=0x0000_3FFF => Some(Self::dev_read(addr, self.via2.read(addr))),
            // Ethernet MAC address PROM
            0x0000_8000..=0x0000_8007 => Some(self.mac_prom(addr)),
            // Ethernet controller, when there is none
            0x0000_A000..=0x0000_B0FF => Some(0),
            // SCC
            0x0000_C000..=0x0000_DFFF => Some(Self::dev_read(addr, self.scc.read(addr >> 1))),
            // Orwell memory controller
            0x0000_E000..=0x0000_EFFF => Some(0),
            // SCSI (NCR 53C96)
            0x0000_F000..=0x0000_F0FF => Some(Self::dev_read(addr, self.scsi.read(addr))),
            // ASC (sound)
            0x0001_4000..=0x0001_5FFF => Some(Self::dev_read(addr, self.asc.read(addr & 0xFFF))),
            // SWIM
            0x0001_E000..=0x0001_FFFF => Some(Self::dev_read(addr, self.swim.read(addr))),
            // Expansion area - unmapped
            _ => None,
        }
    }

    fn io_write_byte(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr & Self::IO_MASK {
            // VIA 1
            0x0000_0000..=0x0000_1FFF => {
                Self::dev_write(addr, val, self.via1.write(addr, val));
                self.sync_via1_pa();
                Some(())
            }
            // VIA 2
            0x0000_2000..=0x0000_3FFF => Self::dev_write(addr, val, self.via2.write(addr, val)),
            // Ethernet MAC address PROM, and the Ethernet controller when there is none
            0x0000_8000..=0x0000_8007 | 0x0000_A000..=0x0000_B0FF => Some(()),
            // SCC
            0x0000_C000..=0x0000_DFFF => Self::dev_write(addr, val, self.scc.write(addr >> 1, val)),
            // Orwell memory controller
            0x0000_E000..=0x0000_EFFF => Some(()),
            // SCSI (NCR 53C96)
            0x0000_F000..=0x0000_F0FF => Self::dev_write(addr, val, self.scsi.write(addr, val)),
            // ASC (sound)
            0x0001_4000..=0x0001_5FFF => {
                Self::dev_write(addr, val, self.asc.write(addr & 0xFFF, val))
            }
            // SWIM
            0x0001_E000..=0x0001_FFFF => Self::dev_write(addr, val, self.swim.write(addr, val)),
            // Expansion area - unmapped
            _ => None,
        }
    }

    /// An access to the I/O area. The VIAs, the SCC, the EASC, the SWIM and
    /// the Ethernet controller are on the I/O bus, behind the JDB and the
    /// Relayer; the 53C96, the DAFB and the MCU are on the system bus
    /// (Quadra 700 Developer Note, chapter 1).
    fn io_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        self.irq = None;
        match acc.addr & Self::IO_MASK {
            // VIA 1 and VIA 2
            0x0000_0000..=0x0000_3FFF => self.via_cycle(acc),
            // Ethernet MAC address PROM ($5000 8000) and controller ($5000 A000): Designing
            // Cards and Drivers for the Macintosh Family, 3rd ed., table 16-5
            0x0000_8000..=0x0000_9FFF => self.io_bus_cycle(acc, 0, false),
            #[cfg(feature = "ethernet")]
            0x0000_A000..=0x0000_BFFF => self.sonic_cycle(acc),
            #[cfg(not(feature = "ethernet"))]
            0x0000_A000..=0x0000_BFFF => self.io_bus_cycle(acc, 0, false),
            // SCC
            0x0000_C000..=0x0000_DFFF => self.io_bus_cycle(acc, SCC_PCLK_WAITS, false),
            // 53C96 pseudo-DMA, through the DAFB
            0x0000_F100..=0x0000_F1FF => self.scsi_dma_cycle(acc),
            // EASC
            0x0001_4000..=0x0001_5FFF => self.io_bus_cycle(acc, 0, false),
            // SWIM
            0x0001_E000..=0x0001_FFFF => self.io_bus_cycle(acc, 0, true),
            // MCU, 53C96
            _ => match self.native_clock.device_sync(&acc) {
                DeviceCycle::Stall(n) => BusResponse::Stall(n),
                DeviceCycle::Access(w) => {
                    bus_access_long(acc, w, self, Self::io_read, Self::io_write)
                }
            },
        }
    }

    /// The Relayer clocks the VIAs at C16M / 20, 783.36 kHz (Developer Note,
    /// "I/O bus adapter ICs"; schematic 050-8020, VIACLK), and their accesses
    /// are synchronous with that clock: one starts on a VIA tick and the CPU
    /// is released half way through the VIA cycle.
    fn via_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        match self.native_clock.via_sync(&acc, C16M_PER_VIA / 2) {
            DeviceCycle::Stall(n) => BusResponse::Stall(n),
            DeviceCycle::Access(w) => bus_access_long(acc, w, self, Self::io_read, Self::io_write),
        }
    }

    /// An access to a device on the I/O bus: it starts on a C16M tick and
    /// lasts a bus cycle in C16M ticks, plus the IO_BUS_WAITS of the
    /// Relayer and the device's own `waits`, in C16M ticks. When
    /// overclocking, the SWIM accesses are also held as far apart as
    /// Swim::hold16() needs.
    fn io_bus_cycle(&mut self, acc: BusAccess<Bus68040>, waits: Ticks, swim: bool) -> BusResponse {
        let w = match self.native_clock.device16_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        let hold = if swim {
            self.swim.hold16(&self.native_clock)
        } else {
            0
        };
        let w = self
            .native_clock
            .scale_dev16_waits(w, IO_BUS_WAITS + waits + hold);
        bus_access_long(acc, w, self, Self::io_read, Self::io_write)
    }

    /// An access to the SONIC registers: an I/O bus device with 16-bit registers, one per long
    /// word, on D15-D0
    #[cfg(feature = "ethernet")]
    fn sonic_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        let w = match self.native_clock.device16_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        let w = self.native_clock.scale_dev16_waits(w, IO_BUS_WAITS);
        bus_access_long(acc, w, self, Self::sonic_read, Self::sonic_write)
    }

    /// The SONIC register at an I/O address, if any
    #[cfg(feature = "ethernet")]
    fn sonic_reg(addr: Address) -> Option<usize> {
        match addr & Self::IO_MASK {
            a @ 0x0000_A000..=0x0000_B0FF => Some(((a - 0xA000) >> 2) as usize),
            _ => None,
        }
    }

    #[cfg(feature = "ethernet")]
    fn sonic_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let reg = self.ethernet.sonic.read(Self::sonic_reg(addr)?);
        // D31-D16 are not connected
        let long = u32::from(reg);
        let shift = 8 * (4 - (addr & 3) - len);
        Some((long >> shift) & (u32::MAX >> (32 - 8 * len)))
    }

    #[cfg(feature = "ethernet")]
    fn sonic_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        let reg = Self::sonic_reg(addr)?;
        if let Some(value) = low_word_written(addr, len, val) {
            self.ethernet.sonic.write(reg, value);
        }
        Some(())
    }

    /// The MAC address PROM of the built-in Ethernet
    fn mac_prom(&self, addr: Address) -> Byte {
        #[cfg(feature = "ethernet")]
        {
            mac_prom_byte(self.ethernet.sonic.port().mac(), (addr & 7) as usize)
        }
        #[cfg(not(feature = "ethernet"))]
        {
            let _ = addr;
            0
        }
    }

    /// A pseudo-DMA access to the 53C96: the DAFB makes it one cycle of the
    /// 16-bit DMA bus, on D31-D16. With the DREQ check on, it waits for DREQ,
    /// which in this model can only come from the CPU, so it times out with
    /// a bus error.
    fn scsi_dma_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        if self.dafb.scsi_drq_check(acc.write) && !self.scsi.get_drq() {
            if !self.scsi_dma_stalled {
                self.scsi_dma_stalled = true;
                return BusResponse::Stall(Self::SCSI_DMA_TIMEOUT);
            }
            self.scsi_dma_stalled = false;
            esp_check!(
                "pseudo-DMA {} at {:08X} size {} timed out: {}",
                if acc.write { "write" } else { "read" },
                acc.addr,
                acc.size,
                self.scsi.dbg()
            );
            return BusResponse::BusError;
        }
        self.scsi_dma_stalled = false;
        if acc.addr & 0xFE != 0 || acc.size == 4 {
            esp_check!(
                "pseudo-DMA {} at {:08X} size {}",
                if acc.write { "write" } else { "read" },
                acc.addr,
                acc.size
            );
        }
        if acc.write && !self.scsi.get_drq() {
            esp_check!("pseudo-DMA write without DREQ: {}", self.scsi.dbg());
        }
        bus_access_word_lane::<4, 0, _, _>(acc, w, self, Self::scsi_dma_read, Self::scsi_dma_write)
    }

    fn scsi_dma_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let before = self.scsi.fifo_len();
        let [b0, b1] = self.scsi.dack_read();
        if len == 1 && before >= 2 {
            esp_check!("byte pseudo-DMA read at {:08X} dropped a byte", addr);
        }
        let word = u32::from_be_bytes([0, 0, b0, b1]);
        Some((word >> (8 * (2 - (addr & 1) - len))) & ((1 << (8 * len)) - 1))
    }

    fn scsi_dma_write(&mut self, _addr: Address, len: u32, val: u32) -> Option<()> {
        // A byte is on both lanes, as the CPU replicates it
        let data = if len == 2 {
            [(val >> 8) as u8, val as u8]
        } else {
            [val as u8; 2]
        };
        self.scsi.dack_write(data);
        Some(())
    }

    /// VIA 2 at power-up. CA2 is PIC_ISM_IRQ*, the SWIM I/O processor
    /// interrupt of the Quadra 900 board design, pulled up and unused on the
    /// Quadra 700 (the 53C96 DREQ goes to the DAFB, not to the VIA).
    fn new_via2(model: MacModel) -> Via2 {
        let mut via2 = Via2::new(model);
        via2.set_line(ViaLine::CA2, LineState::High);
        via2
    }

    /// Syncs the devices driven by the VIA1 port A pins
    fn sync_via1_pa(&mut self) {
        self.swim.sel = self.via1.pa().sel();
        self.swim.intdrive = self.via1.pa().drivesel();
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        if self.mouse_mode == MouseMode::Disabled {
            return;
        }

        if button.is_some() {
            self.via1.adb.event(&AdbEvent::Mouse(MouseEvent {
                button,
                rel_movement: None,
            }));
        }

        if relx == 0 && rely == 0 {
            return;
        }

        match self.mouse_mode {
            MouseMode::Absolute => {
                // Handled through mouse_update_abs()
            }
            MouseMode::RelativeHw => {
                self.via1.adb.event(&AdbEvent::Mouse(MouseEvent {
                    button: None,
                    rel_movement: Some((relx.into(), rely.into())),
                }));
            }
            MouseMode::Disabled => unreachable!(),
        }
    }

    /// Sets the mouse emulation mode
    pub fn set_mouse_mode(&mut self, mode: MouseMode) {
        self.mouse_mode = mode;
    }

    /// Updates the mouse position in absolute mode, given the current value
    /// of RawMouse. Returns what the low memory globals of the OS should hold
    /// for it, if they should change.
    pub fn mouse_update_abs(
        &mut self,
        x: u16,
        y: u16,
        rawmouse: (u16, u16),
    ) -> Option<AbsoluteMouse> {
        if self.mouse_mode != MouseMode::Absolute {
            return None;
        }

        if !self.mouse_ready && rawmouse != (15, 15) {
            // Wait until the boot process has initialized the mouse position so we don't
            // interfere with the memory test.
            return None;
        }
        self.mouse_ready = true;

        // Report updated mouse coordinates to OS
        Some(AbsoluteMouse {
            mtemp: (x, y),
            rawmouse: if self.model >= MacModel::SE {
                // SE+ needs to see even a small difference between the current (RawMouse)
                // and new (MTemp) position, otherwise the change is ignored.
                (x.wrapping_add_signed(-1), y.wrapping_add_signed(1))
            } else {
                (x, y)
            },
        })
    }

    /// Configures emulator speed
    pub fn set_speed(&mut self, speed: EmulatorSpeed) {
        info!("Emulation speed: {:?}", speed);
        self.speed = speed;
    }

    /// Frequency of the bus of the real hardware, without overclocking
    #[inline(always)]
    pub const fn bus_frequency(&self) -> Ticks {
        BUS_FREQUENCY
    }

    /// Sets the frequency of the bus, or goes back to the one of the real
    /// hardware
    pub fn set_overclock(&mut self, bus_frequency: Option<u64>) {
        self.native_clock
            .set_cpu_frequency(bus_frequency.unwrap_or_else(|| self.bus_frequency()));
    }

    /// Programmer's key pressed
    pub fn progkey(&mut self) {
        self.progkey_pressed.set();
    }

    /// The bus with this id, for the emulator's device code
    pub(crate) fn bus_ops(&mut self, bus: &str) -> Option<&mut dyn BusOps> {
        match bus {
            bus_id::SCSI => Some(self.scsi.bus_mut()),
            bus_id::SWIM => Some(&mut self.swim),
            bus_id::NUBUS => Some(&mut self.nubus),
            bus_id::BUILT_IN => Some(self),
            _ => None,
        }
    }

    pub fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode
    }

    /// The display of the machine that the guest's Slot Manager knows by the slot number and
    /// the id of the slot resource of its driver's AuxDCE (`dCtlSlot` and `dCtlSlotId`): the
    /// ids of its bus and slot, and its index among the displays of that device. Called by
    /// the emulator, which reads the guest's list of screens. The driver of the built-in
    /// video has slot 0; the other videos are NuBus cards.
    #[allow(non_snake_case)]
    pub fn display_from_Mac_AuxDCE(
        &self,
        dctl_slot: u8,
        dctl_slot_id: u8,
    ) -> Option<(String, String, usize)> {
        if dctl_slot == 0 {
            return Some((bus_id::BUILT_IN.into(), built_in::VIDEO.into(), 0));
        }
        self.nubus.display_from_Mac_AuxDCE(dctl_slot, dctl_slot_id)
    }

    /// Dispatches a key event to the keyboard
    pub fn keyboard_event(&mut self, ke: KeyEvent) {
        self.via1.adb.event(&AdbEvent::Key(ke));
    }

    /// Releases all pressed inputs
    pub fn input_release_all(&mut self) {
        self.via1.adb.event(&AdbEvent::ReleaseAll);
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.via1.rtc
    }

    /// Sets the file the PRAM is kept in. It is loaded if it exists and created otherwise, then
    /// kept up to date.
    pub fn persist_pram(&mut self, path: &Path) {
        self.rtc_mut().load_pram(path);
    }

    /// Sets the date and time of the clock
    pub fn set_datetime(&mut self, dt: chrono::NaiveDateTime) {
        self.rtc_mut().set_datetime(dt);
    }
}

impl Quadra700Bus {
    /// Wait states of the memories on the system bus (RAM and ROM behind the
    /// MCU, VRAM behind the DAFB): for a single cycle or the first long word
    /// of a burst, and for each further long word of a burst. None for the
    /// other targets, which don't take bursts.
    ///
    /// No timing data is known: these are estimates from the parts (80 ns
    /// fast page mode DRAM, 150 ns ROM without page mode) against the 40 ns
    /// BCLK, averaged over page hits and misses.
    fn memory_waits(region: Region, addr: Address) -> Option<(Ticks, Ticks)> {
        match region {
            Region::Ram => Some((2, 1)),
            Region::Rom => Some((3, 3)),
            Region::Video if (addr & 0xFF_FFFF) < 0x20_0000 => Some((2, 1)),
            _ => None,
        }
    }
}

/// The machine is its `Built-in` bus: a built-in device is one of its fields
impl BusOps for Quadra700Bus {
    fn device(&mut self, slot: &str) -> Option<&mut dyn DeviceOps> {
        match slot {
            built_in::VIDEO => Some(&mut self.dafb),
            #[cfg(feature = "ethernet")]
            built_in::ETHERNET => Some(&mut self.ethernet),
            _ => None,
        }
    }
}

impl Bus<Address, Byte> for Quadra700Bus {
    type Cpu = Bus68040;

    fn get_mask(&self) -> Address {
        0xFFFFFFFF
    }

    fn access(&mut self, acc: BusAccess<Bus68040>) -> Result<BusResponse> {
        let block = (acc.addr >> 24) as usize;
        if self.overlay && (0x40..=0x4F).contains(&block) {
            debug!("Overlay off");
            self.overlay = false;
        }

        let region = MAP[usize::from(self.overlay)][block];
        let memory = Self::memory_waits(region, acc.addr);
        let waits = match memory {
            Some((_, next)) if acc.burst && acc.addr & 0xF != 0 => next,
            Some((first, _)) => first,
            None => 0,
        };

        let mut resp = match region {
            // Everything but RAM and ROM sees the devices up to date, as they are ticked
            Region::SuperSlot | Region::Video | Region::Slot if acc.pending > 0 => {
                BusResponse::Stall(0)
            }
            Region::Ram => bus_access_long(acc, waits, self, Self::ram_read, Self::ram_write),
            Region::Rom => bus_access_long(acc, waits, self, Self::rom_read, Self::ignore_write),
            Region::Io => self.io_cycle(acc),
            Region::ExtRom => bus_access_long(acc, 0, self, Self::ext_rom_read, Self::no_write),
            // What the card of the slot doesn't answer, or an empty slot, stays as
            // superslot_read() and superslot_write() have it
            Region::SuperSlot => match self.nubus.access(acc, NUBUS_WAITS) {
                Some(resp) if resp != BusResponse::BusError => resp,
                _ => bus_access_long(acc, 0, self, Self::superslot_read, Self::superslot_write),
            },
            Region::Video => bus_access_long(acc, waits, self, Self::video_read, Self::video_write),
            Region::Slot => self
                .nubus
                .access(acc, NUBUS_WAITS)
                .unwrap_or(BusResponse::BusError),
            Region::None => BusResponse::BusError,
        };

        if resp == BusResponse::BusError {
            let rw = if acc.write { "write" } else { "read" };
            warn!("Bus error ({}) at unmapped address: {:08X}", rw, acc.addr);
        }
        if acc.burst
            && memory.is_some()
            && let BusResponse::Ack { burst_reject, .. } = &mut resp
        {
            *burst_reject = false;
        }
        Ok(resp)
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        self.irq = None;
        if hard {
            // Clear RAM
            self.ram.fill(0);
            self.disable_memtest();
        }

        // Keep the RTC and ADB for PRAM and event channels
        let Via { adb, rtc, .. } = std::mem::replace(&mut self.via1, Via::new(self.model));
        self.via1.adb = adb;
        self.via1.rtc = rtc;

        self.swim.reset();
        self.scc = Scc::new(SCC_RTXC_FREQUENCY);
        self.via2 = Self::new_via2(self.model);
        self.dafb.reset();
        self.nubus.reset();
        self.asc.reset();
        // The DAFB drives the 53C96 reset
        self.scsi.reset();
        #[cfg(feature = "ethernet")]
        self.ethernet.sonic.reset();
        self.scsi_dma_stalled = false;

        self.amu_active = false;
        self.mouse_ready = false;
        self.overlay = true;
        self.sync_via1_pa();
        Ok(false)
    }

    fn localtalk_poll_needed(&self) -> bool {
        self.scc.localtalk_poll_needed(SccCh::B)
    }
}

impl Tickable for Quadra700Bus {
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
        self.irq = None;
        struct BusEmuContext {
            speed: EmulatorSpeed,
            bus_frequency: Ticks,
        }

        impl EmuContext for BusEmuContext {
            fn speed(&self) -> EmulatorSpeed {
                self.speed
            }

            fn bus_frequency(&self) -> Ticks {
                self.bus_frequency
            }
        }

        let ctx = &BusEmuContext {
            speed: self.speed,
            bus_frequency: self.bus_frequency(),
        };

        // The devices run at the native frequency, whatever the CPU is
        // overclocked to. The VIA clock is C16M / 20 (783.36 kHz).
        let c16m_start = self.native_clock.c16m_ticks();
        let mut native_ticks = 0;
        let mut cpu = ticks;
        while cpu > 0 {
            let step = self.native_clock.advance(cpu, None);
            if step.native == 0 {
                break;
            }
            cpu -= step.cpu_used;
            native_ticks += step.native;
            if step.via_tick {
                self.via1.tick(1, ())?;
                self.via2.tick(1, ())?;

                // The 60.15 Hz interrupt of VIA 1 is the Timer 1 output of VIA 2
                self.via1
                    .set_line(ViaLine::CA1, self.via2.pb().v2vbl().into());
            }
        }

        // Pacing at 60 Hz
        self.vblank_clock += native_ticks;
        while self.vblank_clock >= ctx.bus_frequency / 60 {
            self.vblank_clock -= ctx.bus_frequency / 60;

            match self.speed {
                EmulatorSpeed::Video => {
                    // Sync to 60 fps video
                    let frametime = self.vblank_time.elapsed().as_micros() as u64;
                    const DESIRED_FRAMETIME: u64 = 1_000_000 / 60;
                    self.vblank_time = Instant::now();
                    if frametime < DESIRED_FRAMETIME {
                        thread::sleep(Duration::from_micros(DESIRED_FRAMETIME - frametime));
                    }
                }
                EmulatorSpeed::FastForward(max_speed) => {
                    // Cap to the requested speedup multiplier
                    let frametime = self.vblank_time.elapsed().as_micros() as u64;
                    let desired_frametime = (1_000_000.0 / (60.0 * max_speed)) as u64;
                    self.vblank_time = Instant::now();
                    if frametime < desired_frametime {
                        thread::sleep(Duration::from_micros(desired_frametime - frametime));
                    }
                }
                _ => {}
            }
        }

        self.scc.tick(native_ticks, ctx.bus_frequency);

        // Audio
        // The interrupt output of the EASC is active low
        self.via2.set_line(ViaLine::CB1, (!self.asc.irq()).into());

        self.asc_clock += native_ticks;
        while self.asc_clock >= ctx.bus_frequency / self.asc.sample_rate() {
            self.asc_clock -= ctx.bus_frequency / self.asc.sample_rate();

            self.asc.tick(self.speed == EmulatorSpeed::Accurate)?;
        }

        // Built-in video. Interrupt is wired to the slot $F line of VIA 2
        self.dafb.tick(native_ticks, ctx)?;
        let video_irq = self.dafb.get_irq();
        self.via2.a_in.set_v2irqvideo(!video_irq);

        let mut dma = Dma {
            ram: &mut self.ram,
            overlay: self.overlay,
            writes: &mut self.dma_writes,
        };

        // Built-in Ethernet. In the Macintosh interrupt scheme, its interrupt is on PA0 of
        // VIA 2, "Ethernet IRQ"; in the A/UX scheme it is a level 3 interrupt of its own
        // (Macintosh Quadra 700 Developer Note, tables 2-6 and 2-7).
        #[cfg(feature = "ethernet")]
        let ethernet_irq = {
            self.ethernet
                .sonic
                .tick(native_ticks, ctx.bus_frequency, &mut dma);
            self.ethernet.sonic.irq() && self.via1.pb().aux_inten()
        };
        #[cfg(not(feature = "ethernet"))]
        let ethernet_irq = false;

        // The interrupt of slot $D is on PA4 and the one of slot $E on PA5 (Designing Cards
        // and Drivers for the Macintosh Family, 3rd ed., table 16-6)
        let mut nubus_dma = NubusDma {
            dma: &mut dma,
            nubus_waits: NUBUS_WAITS,
        };
        self.nubus.tick(native_ticks, ctx, &mut nubus_dma)?;
        let slot_irqs = self.nubus.irqs() | u8::from(ethernet_irq);
        self.via2.a_in.set_v2irqs(!slot_irqs);

        // An OR gate of the slot, Ethernet and video interrupts makes /SLOTIRQ (same chapter)
        self.via2.set_line(
            ViaLine::CA1,
            if video_irq || slot_irqs != 0 {
                LineState::Low
            } else {
                LineState::High
            },
        );

        self.scsi.tick(native_ticks, ctx)?;

        // The interrupt output of the 53C96 is active low
        self.via2
            .set_line(ViaLine::CB2, (!self.scsi.get_irq()).into());

        // The SWIM is on the I/O bus, clocked by C16M
        self.swim.intdrive = self.via1.pa().drivesel();
        self.swim
            .tick(self.native_clock.c16m_ticks() - c16m_start, ())?;

        Ok(1)
    }
}

impl Quadra700Bus {
    fn compute_irq(&mut self) -> Option<u8> {
        if self.progkey_pressed.get_clear() {
            return Some(7);
        }

        // The interrupt PAL (schematic U55) has a second set of levels for
        // A/UX, selected by VIA1 PB6 (Developer Note table 2-7)
        let pb = self.via1.pb();
        // Log when these change, to see which systems use them
        let aux_irq_mode = !pb.aux_inten();
        if self.aux_irq_mode != aux_irq_mode {
            self.aux_irq_mode = aux_irq_mode;
            log::warn!(
                "Interrupt PAL switched to the {} levels",
                if self.aux_irq_mode {
                    "A/UX"
                } else {
                    "Macintosh"
                }
            );
        }
        let soft_irq = !pb.soft_irq();
        if self.soft_irq != soft_irq {
            self.soft_irq = soft_irq;
            log::warn!(
                "Software interrupt {}",
                if self.soft_irq {
                    "asserted"
                } else {
                    "released"
                }
            );
        }
        if !pb.aux_inten() {
            #[cfg(feature = "ethernet")]
            let ethernet_irq = self.ethernet.sonic.irq();
            #[cfg(not(feature = "ethernet"))]
            let ethernet_irq = false;
            return if self.via1.irq() {
                Some(6)
            } else if self.asc.irq() {
                Some(5)
            } else if self.scc.get_irq() {
                Some(4)
            } else if ethernet_irq {
                Some(3)
            } else if self.via2.irq() {
                Some(2)
            } else if !pb.soft_irq() {
                Some(1)
            } else {
                None
            };
        }

        if self.scc.get_irq() {
            return Some(4);
        }
        if self.via2.irq() {
            return Some(2);
        }
        if self.via1.irq() {
            return Some(1);
        }

        None
    }
}

impl IrqSource for Quadra700Bus {
    fn get_irq(&mut self) -> Option<u8> {
        if let Some(irq) = self.irq {
            return irq;
        }
        let irq = self.compute_irq();
        self.irq = Some(irq);
        irq
    }
}

impl InspectableBus<Address, Byte> for Quadra700Bus {
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        // Everything up to 0x4FFFFFFF is safe (RAM/ROM only)
        let val = match MAP[usize::from(self.overlay)][(addr >> 24) as usize] {
            Region::Ram => self.ram_read(addr, 1),
            Region::Rom => self.rom_read(addr, 1),
            _ => None,
        };
        val.map(|v| v as Byte)
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        // Everything up to 0x4FFFFFFF is safe (RAM/ROM only)
        match MAP[usize::from(self.overlay)][(addr >> 24) as usize] {
            Region::Ram => self.ram_write(addr, 1, val.into()),
            Region::Rom => Some(()),
            _ => None,
        }
    }

    fn inspect_read_bulk(&mut self, addr: Address, out: &mut [Byte]) -> InspectRun {
        const BLOCK: Address = 1 << 24;
        let offset = (MAP[usize::from(self.overlay)][(addr >> 24) as usize] == Region::Ram
            && (addr as usize) < self.ram.len())
        .then_some(addr as usize);
        if let Some(idx) = offset {
            let max = out
                .len()
                .min((BLOCK - (addr & (BLOCK - 1))) as usize)
                .min(self.ram.len() - idx);
            out[..max].copy_from_slice(&self.ram[idx..idx + max]);
            return InspectRun::Data(max);
        }
        inspect_read_bytes(self, addr, out, BLOCK)
    }

    fn inspect_generation(&self) -> u32 {
        (self.dma_writes << 1) | u32::from(self.overlay)
    }
}

impl Debuggable for Quadra700Bus {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        vec![
            dbgprop_nest!("Apple Desktop Bus", self.via1.adb),
            dbgprop_nest!("Enhanced Apple Sound Chip", self.asc),
            dbgprop_nest!("SCSI controller (NCR 53C96)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA 1 (SY6522)", self.via1),
            dbgprop_nest!("VIA 2 (SY6522)", self.via2),
        ]
    }
}

#[cfg(all(test, feature = "ethernet"))]
mod tests {
    use super::*;
    use crate::devices::{EthernetConfig, EthernetLinkType, FunctionConfig};

    #[test]
    fn the_built_in_ethernet_answers_on_the_io_bus() {
        let rom = vec![0; 1024 * 1024];
        let devices = [DeviceConfig {
            bus: bus_id::BUILT_IN.into(),
            slot: built_in::ETHERNET.into(),
            model: Some(DeviceModel::BuiltInSonic),
            rom: None,
            function: FunctionConfig::Ethernet(EthernetConfig {
                mac: Some([0x08, 0x00, 0x07, 0x12, 0x34, 0x56]),
                link: EthernetLinkType::Down,
            }),
        }];
        let machine = MachineConfig::new(&rom, MacModel::Quadra700);
        let mut bus = Quadra700Bus::new(&machine, &devices).unwrap();

        // The PROM has the bytes of the address with their bits reversed
        let prom: Vec<_> = (0..6)
            .map(|i| bus.io_read_byte(0x50F0_8000 + i).unwrap())
            .collect();
        assert_eq!(prom, [0x10, 0x00, 0xE0, 0x48, 0x2C, 0x6A]);

        // A register is the low word of its long: the silicon revision, register $28, and
        // the start of the resource area, register $15
        assert_eq!(
            bus.sonic_read(0x50F0_A000 + 4 * 0x28, 4),
            Some(u32::from(SONIC_REVISION))
        );
        bus.sonic_write(0x50F0_A000 + 4 * 0x15 + 2, 2, 0x1234);
        assert_eq!(bus.sonic_read(0x50F0_A000 + 4 * 0x15 + 2, 2), Some(0x1234));
        assert_eq!(bus.sonic_read(0x50F0_A000 + 4 * 0x15, 2), Some(0));
    }
}
