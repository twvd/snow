//! Macintosh Quadra 800: IOSB and djMEMC
//!
//! IOSB holds VIA 1, VIA 2 (a real VIA core without timers or shift register),
//! the SWIM II and the sound, decodes the I/O area and bridges the CPU bus to
//! the I/O bus, where the 53C96 and the SCC are. It is very close to PrimeTime
//! in the LC 475, but ADB goes through a transceiver on VIA 1 and the clock chip
//! is on VIA 1 port B, as on the Quadra 700 (schematic sheets 15 and 19).
//! djMEMC holds the memory timing and the video, which is DAFB compatible.
//!
//! The NuBus controller (Kiwi) and its three slots, the PDS and the Ethernet
//! controller are not there yet. An access to the Ethernet controller is a bus
//! error, which the ROM takes for a machine without the Ethernet option.

use std::thread;
use std::time::{Duration, Instant};

use crate::bus::access::{Bus68040, BusAccess, BusResponse, bus_access_long};
use crate::bus::native_clock::{C16M_FREQUENCY, DeviceCycle, NativeClock};
use crate::bus::{
    Address, Bus, BusMember, InspectRun, InspectableBus, IrqSource, inspect_read_bytes,
};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::macii::via2::Via2;
use crate::mac::quadra::dafb::{Dafb, DafbKind};
use crate::mac::rtc::Rtc;
use crate::mac::scc::{Scc, SccCh};
use crate::mac::scsi::esp::{Esp, esp_check};
use crate::mac::sound::v8_sonora::{V8Sound, V8Variant};
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::via_common::{IRQ_CA2, LineState, ViaLine};
use crate::mac::{AbsoluteMouse, MacModel, MacMonitor};
use crate::renderer::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, Renderer, null_audio_sink,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

/// Frequency of the bus of the real hardware, without overclocking: the
/// 33.33 MHz of the 68040 (Developer Note)
const BUS_FREQUENCY: Ticks = 33_333_333;

/// Long at the end of the I/O space which identifies an IOSB machine: the VIA 1
/// ID pins tell which one
const CPU_ID: u32 = 0xA55A_2BAD;
const CPU_ID_ADDR: Address = 0x5FFF_FFFC;

/// Period of the 60.15 Hz interrupt of IOSB on VIA 1 CA1, in VIA ticks
const TICK60_PERIOD: Ticks = 13024;

/// Wait states, in C16M ticks, of an access to a device on the I/O bus, on top
/// of the bus cycle itself: IOSB synchronises the cycle with the I/O bus clock
/// and its termination back with the CPU clock. Fit to BusTiming on a 25 MHz
/// Quadra 610, which has IOSB too: VIA 2 and the ISM read in 12 clocks, the
/// 53C96 in 12 and the ASC in 13. No Quadra 800 has been measured.
const IO_BUS_WAITS: Ticks = 5;

/// Extra wait states, in C16M ticks, of an SCC access, for PCLK: the Quadra 610
/// reads it in 17 clocks. The recovery time between accesses isn't modelled.
const SCC_PCLK_WAITS: Ticks = 3;

/// C16M ticks from the VIA 1 tick that an access starts on to the CPU's release:
/// BusTiming holds the CPU 0.76 VIA cycle on a Quadra 610
const VIA_RELEASE: Ticks = 15;

/// Clock of the RTxCA and RTxCB pins of the SCC, 3.6864 MHz, which the baud
/// rate generators run from
const SCC_RTXC_FREQUENCY: Ticks = 3_686_400;

/// Sizes of video RAM which can be fitted: 512 KB on the board and sockets for
/// another 512 KB (Developer Note)
pub const VRAM_SIZES: &[usize] = &[512 * 1024, 1024 * 1024];

/// Bytes of the IOSB registers kept
const IOSB_REGS_SIZE: usize = 0x80;

/// Banks of RAM that djMEMC decodes. Banks 0 and 1 are the 4 MB on the board,
/// one on each data bus (schematic sheet 6). The four SIMM sockets have two
/// banks each, one per side (sheet 7).
const RAM_BANKS: usize = 10;
const BOARD_BANKS: usize = 2;
const BOARD_BANK_SIZE: usize = 4 * 1024 * 1024;
/// Sizes of a bank of a SIMM: 1M x 4 or 4M x 4 DRAMs
const SIMM_BANK_SIZES: [usize; 2] = [16 * 1024 * 1024, 4 * 1024 * 1024];

/// Registers of djMEMC, one every 4 bytes, 12 bits wide
const MEMC_REGS: usize = 16;
/// Interleave enables, and the version of the chip in bits 11-9, read only
const MEMC_INTERLEAVE: usize = 0;
const MEMC_VERSION: u16 = 0xE00;
/// RAM banks: bits 7-0 are A29-A22 of the base, bit 8 the DRAM addressing mode
const MEMC_BANK0: usize = 1;
const MEMC_BANK_BASE: u16 = 0xFF;
/// Their values at power up: bank n at n x 64 MB
#[rustfmt::skip]
const MEMC_RESET: [u16; MEMC_REGS] = [
    // Interleave
    0x000,
    // RAM banks
    0x000, 0x010, 0x020, 0x030, 0x040, 0x050, 0x060, 0x070, 0x080, 0x090,
    // Top of the RAM
    0x0FF,
    // Configuration
    0x0FF,
    // Refresh interval
    0x081,
    0,
    0,
];

/// RAM refresh: every 15.6 us, keeping the RAM busy for 6 clocks
const RAM_REFRESH_PERIOD: Ticks = 520;
const RAM_REFRESH_WAITS: Ticks = 6;

/// RAM decode granularity, the unit of the bank bases, and number of such
/// blocks in the RAM space
const RAM_BLOCK_SHIFT: u32 = 22;
const RAM_BLOCKS: usize = 256;

/// Marks a RAM decode block with nothing in it
const RAM_BLOCK_NONE: u32 = u32::MAX;

/// What answers in a 16MB block of the address space
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    None,
    Ram,
    /// ROM, also in place of the RAM during the boot overlay
    Rom,
    /// I/O devices, repeating every 256KB
    Io,
    /// Machine identification
    CpuId,
    /// Built-in video (djMEMC), slot $9
    Video,
}

/// The NuBus slots $C-$E and the PDS are empty: nothing answers there, which
/// is a bus error, as the Slot Manager expects.
const fn region(overlay: bool, block: usize) -> Region {
    match block {
        0x00..=0x3F if overlay => Region::Rom,
        0x00..=0x3F => Region::Ram,
        0x40..=0x4F => Region::Rom,
        0x50..=0x53 => Region::Io,
        0x5F => Region::CpuId,
        0xF9 => Region::Video,
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
#[serde(bound = "")]
pub struct Quadra800Bus<TRenderer: Renderer> {
    /// Interrupt level, until a tick of the bus or an access to the I/O area
    #[serde(skip)]
    irq: Option<Option<u8>>,

    /// The currently emulated Macintosh model
    model: MacModel,

    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    pub(crate) via1: Via,
    pub(crate) via2: Via2,
    pub(crate) scc: Scc,
    pub(crate) sound: V8Sound,
    /// C16M ticks into the current sound sample
    sound_clock: Ticks,
    /// VIA ticks into the 60.15 Hz period
    tick60_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    /// C16M tick the SWIM can be accessed again
    swim_ready: Ticks,
    pub(crate) scsi: Esp,
    /// A pseudo-DMA access is held off, waiting for DREQ
    scsi_dma_stalled: bool,
    /// Halves of a pseudo-DMA long already moved, while the access is held off
    scsi_dma_done: u32,
    /// Data of a pseudo-DMA read
    scsi_dma_result: u32,
    /// Registers of IOSB, which are only stored: they set up the timing of the
    /// 53C96 accesses
    iosb_regs: Vec<u8>,
    /// Registers of djMEMC, only stored apart from the base of the RAM banks.
    /// The timing doesn't follow the configuration, nor the DRAM addressing
    /// mode of the banks. Interleaving isn't modelled: the ROM only turns it
    /// on when the chip reports a version, and this one reports 0.
    memc_regs: [u16; MEMC_REGS],
    /// Native tick of the next RAM refresh
    next_refresh: Ticks,
    /// Row of the VRAM that was accessed last
    vram_row: Address,
    /// Offset in `ram` of each block of the RAM space, as the banks decode it
    ram_map: Vec<u32>,

    rom_mask: usize,

    overlay: bool,

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

    /// Built-in video
    pub(crate) dafb: Dafb<TRenderer>,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer> Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Value to return on open bus
    /// Certain applications (e.g. Animation Toolkit) rely on this.
    const OPENBUS: u8 = 0;

    const IO_MASK: Address = 0x3_FFFF;

    /// CPU cycles before a pseudo-DMA access waiting for DREQ ends with a bus
    /// error. The length of this timeout is not documented.
    const SCSI_DMA_TIMEOUT: Ticks = 640;

    pub fn new(
        model: MacModel,
        rom: &[u8],
        renderer: TRenderer,
        monitor: MacMonitor,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
        vram_size: Option<usize>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());

        let mut bus = Self {
            irq: None,
            model,

            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            via1: Via::new(model),
            via2: Via2::new(model),
            scc: Scc::new(SCC_RTXC_FREQUENCY),
            swim: Swim::new_swim2(model.fdd_drives(), C16M_FREQUENCY),
            swim_ready: 0,
            // MODE pins strapped for a 16-bit DMA bus, as on the LC 475
            scsi: Esp::new(true),
            scsi_dma_stalled: false,
            scsi_dma_done: 0,
            scsi_dma_result: 0,
            iosb_regs: vec![0; IOSB_REGS_SIZE],
            memc_regs: MEMC_RESET,
            next_refresh: 0,
            vram_row: 0,
            ram_map: vec![RAM_BLOCK_NONE; RAM_BLOCKS],
            sound: V8Sound::new(V8Variant::PrimeTime),
            sound_clock: 0,
            tick60_clock: 0,
            mouse_ready: false,

            rom_mask: rom.len() - 1,

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(BUS_FREQUENCY),
            vblank_time: Instant::now(),
            vblank_clock: 0,
            progkey_pressed: LatchingEvent::default(),

            dafb: Dafb::new(
                renderer,
                monitor,
                DafbKind::MemcJr,
                vram_size.unwrap_or(VRAM_SIZES[1]),
            ),
            mouse_mode,
        };

        bus.via1.adb.add_device(AdbMouse::new());
        bus.via1.adb.add_device(AdbKeyboard::new());

        bus.ram_map_update();
        bus
    }

    /// Sizes of the RAM banks for an amount of RAM: the board, then the SIMMs
    /// from bank 2 up, 16 MB per bank while there is enough, else 4 MB
    fn ram_bank_sizes(ram_size: usize) -> [usize; RAM_BANKS] {
        let mut sizes = [0; RAM_BANKS];
        let mut left = ram_size;
        for (bank, size) in sizes.iter_mut().enumerate() {
            *size = if bank < BOARD_BANKS {
                BOARD_BANK_SIZE.min(left)
            } else {
                SIMM_BANK_SIZES
                    .into_iter()
                    .find(|&s| s <= left)
                    .unwrap_or(0)
            };
            left -= *size;
        }
        sizes
    }

    /// First 4 MB block of a bank in the RAM space
    fn ram_bank_base(&self, bank: usize) -> usize {
        usize::from(self.memc_regs[MEMC_BANK0 + bank] & MEMC_BANK_BASE)
    }

    /// Rebuilds the decode of the RAM space. The banks are in `ram` in their
    /// order. An address belongs to the last bank which starts at or below it,
    /// and past the size of the bank nothing answers.
    fn ram_map_update(&mut self) {
        let sizes = Self::ram_bank_sizes(self.ram.len());
        let offsets: Vec<usize> = sizes
            .iter()
            .scan(0, |offset, &size| {
                let this = *offset;
                *offset += size;
                Some(this)
            })
            .collect();
        for block in 0..RAM_BLOCKS {
            let bank = (0..RAM_BANKS)
                .rev()
                .find(|&b| self.ram_bank_base(b) <= block);
            self.ram_map[block] = bank.map_or(RAM_BLOCK_NONE, |bank| {
                let inside = (block - self.ram_bank_base(bank)) << RAM_BLOCK_SHIFT;
                if inside < sizes[bank] {
                    (offsets[bank] + inside) as u32
                } else {
                    RAM_BLOCK_NONE
                }
            });
        }
    }

    /// Offset in `ram` of an address in the RAM space
    #[inline(always)]
    fn ram_index(&self, addr: Address) -> Option<usize> {
        let offset = self.ram_map[((addr >> RAM_BLOCK_SHIFT) as usize) & (RAM_BLOCKS - 1)];
        (offset != RAM_BLOCK_NONE)
            .then(|| offset as usize + (addr as usize & ((1 << RAM_BLOCK_SHIFT) - 1)))
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        self.dafb.renderer = Some(renderer);
        self.dafb.render().unwrap();

        self.sound.after_deserialize();
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
                (C16M_FREQUENCY / self.sound.sample_period()) as i32,
                AUDIO_CHANNELS as u8,
                AUDIO_BUFFER_SAMPLES as u16,
            )
            .unwrap_or_else(|_| null_audio_sink());
        self.sound.set_sink(sink);

        self.scsi.bus_mut().set_audio_provider(provider)
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
    // one RAM page and one bank

    fn ram_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let bytes = self
            .ram_index(addr)
            .and_then(|idx| self.ram.get(idx..idx + len as usize));
        // Nothing there: ignored silently, as the ROM looks for the end of RAM
        let Some(bytes) = bytes else {
            return Some(0);
        };
        Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)))
    }

    fn ram_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        if let Some(idx) = self.ram_index(addr)
            && let Some(ram) = self.ram.get_mut(idx..idx + len as usize)
        {
            ram.copy_from_slice(&val.to_be_bytes()[4 - len as usize..]);
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

    fn ignore_write(&mut self, _addr: Address, _len: u32, _val: u32) -> Option<()> {
        Some(())
    }

    fn cpu_id_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let bytes = CPU_ID.to_be_bytes();
        (addr..addr + len).try_fold(0, |v, a| {
            let idx = a.checked_sub(CPU_ID_ADDR)? as usize;
            Some((v << 8) | u32::from(*bytes.get(idx)?))
        })
    }

    /// The video is reached a byte at a time. djMEMC passes the 12 bits of its
    /// registers on the data bus, without the holding register of MEMCjr.
    fn video_read(&mut self, addr: Address, len: u32) -> Option<u32> {
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
            // SCC
            0x0000_C000..=0x0000_DFFF => Some(Self::dev_read(addr, self.scc.read(addr >> 1))),
            // djMEMC
            0x0000_E000..=0x0000_E03F => Some(self.memc_read(addr)),
            0x0000_E040..=0x0000_FFFF => Some(0),
            // SCSI (NCR 53C96)
            0x0001_0000..=0x0001_00FF => Some(Self::dev_read(addr, self.scsi.read(addr))),
            // Sound
            0x0001_4000..=0x0001_4FFF => Some(Self::dev_read(addr, self.sound.read(addr & 0xFFF))),
            // IOSB registers
            0x0001_8000..=0x0001_9FFF => Some(self.iosb_regs[Self::iosb_reg(addr)]),
            // SWIM
            0x0001_E000..=0x0001_FFFF => Some(Self::dev_read(addr, self.swim.read(addr))),
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
            // SCC
            0x0000_C000..=0x0000_DFFF => Self::dev_write(addr, val, self.scc.write(addr >> 1, val)),
            // djMEMC
            0x0000_E000..=0x0000_E03F => {
                self.memc_write(addr, val);
                Some(())
            }
            0x0000_E040..=0x0000_FFFF => Some(()),
            // SCSI (NCR 53C96)
            0x0001_0000..=0x0001_00FF => Self::dev_write(addr, val, self.scsi.write(addr, val)),
            // Sound
            0x0001_4000..=0x0001_4FFF => {
                Self::dev_write(addr, val, self.sound.write(addr & 0xFFF, val))
            }
            // IOSB registers
            0x0001_8000..=0x0001_9FFF => {
                self.iosb_regs[Self::iosb_reg(addr)] = val;
                Some(())
            }
            // SWIM
            0x0001_E000..=0x0001_FFFF => Self::dev_write(addr, val, self.swim.write(addr, val)),
            _ => None,
        }
    }

    /// Number of a djMEMC register
    fn memc_reg(addr: Address) -> usize {
        ((addr & Self::IO_MASK) as usize - 0xE000) / 4
    }

    /// A byte of a djMEMC register: its 12 bits are the low ones of the long
    fn memc_read(&self, addr: Address) -> Byte {
        let val = self.memc_regs[Self::memc_reg(addr)];
        match addr & 3 {
            2 => (val >> 8) as Byte,
            3 => val as Byte,
            _ => 0,
        }
    }

    fn memc_write(&mut self, addr: Address, val: Byte) {
        let reg = Self::memc_reg(addr);
        let old = self.memc_regs[reg];
        let new = match addr & 3 {
            2 => (old & 0xFF) | (u16::from(val & 0xF) << 8),
            3 => (old & 0xF00) | u16::from(val),
            _ => return,
        };
        match reg {
            MEMC_INTERLEAVE => self.memc_regs[reg] = new & !MEMC_VERSION,
            MEMC_BANK0..=10 => {
                self.memc_regs[reg] = new;
                self.ram_map_update();
            }
            _ => self.memc_regs[reg] = new,
        }
    }

    /// Index of an IOSB register byte: the registers are 16 bits wide, one
    /// every $100 bytes
    fn iosb_reg(addr: Address) -> usize {
        ((addr as usize >> 7) & !1 | (addr as usize & 1)) % IOSB_REGS_SIZE
    }

    /// An access to the I/O area. IOSB completes all the cycles here, also to
    /// the 53C96 and the SCC, which it bridges to the I/O bus. djMEMC is on the
    /// CPU bus.
    fn io_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        self.irq = None;
        match acc.addr & Self::IO_MASK {
            // VIA 1. VIA 2 has no timers, and doesn't take the VIA clock either:
            // its reads take the same time at any spacing.
            0x0000_0000..=0x0000_1FFF => self.via_cycle(acc),
            0x0000_2000..=0x0000_3FFF => self.io_bus_cycle(acc, 0, false),
            // SCC
            0x0000_C000..=0x0000_DFFF => self.io_bus_cycle(acc, SCC_PCLK_WAITS, false),
            // 53C96 pseudo-DMA
            0x0001_0100..=0x0001_0103 => self.scsi_dma_cycle(acc),
            // 53C96, sound, IOSB registers
            0x0001_0000..=0x0001_00FF | 0x0001_4000..=0x0001_4FFF | 0x0001_8000..=0x0001_9FFF => {
                self.io_bus_cycle(acc, 0, false)
            }
            // SWIM
            0x0001_E000..=0x0001_FFFF => self.io_bus_cycle(acc, 0, true),
            // djMEMC, and nothing
            _ => match self.native_clock.device_sync(&acc) {
                DeviceCycle::Stall(n) => BusResponse::Stall(n),
                DeviceCycle::Access(w) => {
                    bus_access_long(acc, w, self, Self::io_read, Self::io_write)
                }
            },
        }
    }

    /// The VIAs are clocked at C16M / 20, 783.36 kHz, and their accesses are
    /// synchronous with that clock: one starts on a VIA tick and the CPU is
    /// released part way through the VIA cycle.
    fn via_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        match self.native_clock.via_sync(&acc, VIA_RELEASE) {
            DeviceCycle::Stall(n) => BusResponse::Stall(n),
            DeviceCycle::Access(w) => bus_access_long(acc, w, self, Self::io_read, Self::io_write),
        }
    }

    /// An access to a device on the I/O bus: it starts on a C16M tick and
    /// lasts a bus cycle in C16M ticks, plus the IO_BUS_WAITS of IOSB and the
    /// device's own `waits`, in C16M ticks. The SWIM accesses are held as far
    /// apart as the ISM needs (Swim::ism_recovery()) and, when overclocking, as
    /// Swim::hold16() needs.
    fn io_bus_cycle(&mut self, acc: BusAccess<Bus68040>, waits: Ticks, swim: bool) -> BusResponse {
        let w = match self.native_clock.device16_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        let hold = if swim { self.swim_hold() } else { 0 };
        let w = self
            .native_clock
            .scale_dev16_waits(w, IO_BUS_WAITS + waits + hold);
        bus_access_long(acc, w, self, Self::io_read, Self::io_write)
    }

    /// C16M ticks a SWIM access is held for, called after device16_sync(). FCLK
    /// is taken as C16M.
    fn swim_hold(&mut self) -> Ticks {
        let now = self.native_clock.c16m_ticks();
        let hold = self
            .swim_ready
            .saturating_sub(now)
            .max(self.swim.hold16(&self.native_clock));
        self.swim_ready = now + hold + self.swim.ism_recovery();
        hold
    }

    /// A pseudo-DMA access to the 53C96. IOSB holds the cycle off until DREQ,
    /// then makes one cycle of the 16-bit DMA bus for a word, on D31-D16, and
    /// two for a long, high half first (MAME; no real hardware data). A long
    /// is one bus transfer for the 68040, so a hold-off between the halves is
    /// IOSB's too: when the second half isn't there yet, the first was already
    /// moved and only the second is waited for.
    fn scsi_dma_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        let halves = if acc.size == 4 { 2 } else { 1 };
        while self.scsi_dma_done < halves {
            if !self.scsi.get_drq() {
                if !self.scsi_dma_stalled {
                    self.scsi_dma_stalled = true;
                    return BusResponse::Stall(Self::SCSI_DMA_TIMEOUT);
                }
                self.scsi_dma_stalled = false;
                self.scsi_dma_done = 0;
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
            self.scsi_dma_check(&acc);
            self.scsi_dma_transfer(acc.write, acc.data, acc.size);
            self.scsi_dma_done += 1;
        }
        self.scsi_dma_done = 0;
        bus_access_long(acc, w, self, Self::scsi_dma_read, Self::ignore_write)
    }

    /// Warns of the accesses the model only guesses at: +2 and +3, and bytes, as
    /// a cycle of the DMA bus moves two unless only one is left. MAME doesn't take
    /// byte reads or anything at +2.
    fn scsi_dma_check(&self, acc: &BusAccess<Bus68040>) {
        let left = if acc.write {
            self.scsi.tc() as usize
        } else {
            self.scsi.fifo_len()
        };
        if acc.addr & 2 != 0 || (acc.size == 1 && left >= 2) {
            esp_check!(
                "pseudo-DMA {} at {:08X} size {}: {}",
                if acc.write { "write" } else { "read" },
                acc.addr,
                acc.size,
                self.scsi.dbg()
            );
        }
    }

    /// One cycle of the DMA bus. A word or a read is on the upper half of the
    /// long data bus, the second half of a long on the lower one; a byte write
    /// is on both lanes, as the CPU replicates it.
    fn scsi_dma_transfer(&mut self, write: bool, data: u32, size: u32) {
        let second = self.scsi_dma_done == 1;
        if write {
            let half = if second {
                data
            } else {
                data >> (8 * (size.saturating_sub(2)))
            };
            let bytes = if size == 1 {
                [data as u8; 2]
            } else {
                [(half >> 8) as u8, half as u8]
            };
            self.scsi.dack_write(bytes);
        } else {
            let [b0, b1] = self.scsi.dack_read();
            let half = u32::from(u16::from_be_bytes([b0, b1]));
            self.scsi_dma_result = if second {
                (self.scsi_dma_result & 0xFFFF_0000) | half
            } else {
                // Not in a manual: NetBSD's esp.c, from real machines it doesn't
                // name, has word reads in a loop alternate data and ones: the
                // word at +2 handshakes too and reads $FFFF.
                (half << 16) | 0xFFFF
            };
        }
    }

    fn scsi_dma_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let shift = 8 * (4 - (addr & 3) - len);
        let mask = if len == 4 { !0 } else { (1 << (8 * len)) - 1 };
        Some((self.scsi_dma_result >> shift) & mask)
    }

    /// Syncs the devices driven by the VIA1 port A pins
    fn sync_via1_pa(&mut self) {
        self.swim.sel = self.via1.pa().sel();
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

        // Report updated mouse coordinates to OS. The current (RawMouse) and
        // new (MTemp) positions are the same: as on the LC III, the ROM takes
        // any difference between them for a relative movement.
        Some(AbsoluteMouse {
            mtemp: (x, y),
            rawmouse: (x, y),
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

    pub fn video_blank(&mut self) -> Result<()> {
        self.dafb.blank()
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
}

impl<TRenderer> Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Wait states of the memories on the CPU bus (RAM and ROM, VRAM behind
    /// djMEMC): for a single cycle or the first long word of a burst, and for
    /// each further long word of a burst. None for the other targets, which
    /// don't take bursts. These are the LC 475's at 25 MHz: RAM 5-2-2-2 reading
    /// and 4-2-2-2 writing, ROM 5, which BusTiming also reads on a 25 MHz
    /// Quadra 610 for single accesses. Real writes are posted, which the bus
    /// can't express: only their period is right.
    fn memory_waits(region: Region, addr: Address, write: bool) -> Option<(Ticks, Ticks)> {
        match region {
            Region::Ram => Some((3, 1)),
            Region::Rom => Some((3, 3)),
            Region::Video if (addr & 0xFF_FFFF) < 0x20_0000 => Some((2, if write { 1 } else { 2 })),
            _ => None,
        }
    }

    /// Catches up with the RAM refreshes, nearly always a single one. Returns
    /// how long an access at this time waits for the last.
    #[cold]
    fn ram_refresh(&mut self, now: Ticks) -> Ticks {
        let late = (now - self.next_refresh) % RAM_REFRESH_PERIOD;
        self.next_refresh = now - late + RAM_REFRESH_PERIOD;
        RAM_REFRESH_WAITS.saturating_sub(late)
    }

    /// Further wait states of an access to the VRAM which opens another row.
    /// A row is 1 KB. Each of the two banks really has its own, and the screen
    /// refresh transfers close them too.
    fn vram_row_waits(&mut self, addr: Address) -> Ticks {
        let row = (addr & 0xFF_FFFF) >> 10;
        if std::mem::replace(&mut self.vram_row, row) == row {
            0
        } else {
            3
        }
    }
}

impl<TRenderer> Bus<Address, Byte> for Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
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
        let memory = Self::memory_waits(region, acc.addr, acc.write);
        let waits = match memory {
            Some((_, next)) if acc.burst && acc.addr & 0xF != 0 => next,
            Some((first, _)) if matches!(region, Region::Video) => {
                first + self.vram_row_waits(acc.addr)
            }
            Some((first, _)) => first,
            None => 0,
        };

        let mut resp = match region {
            // Everything but RAM and ROM sees the devices up to date, as they are ticked
            Region::CpuId | Region::Video if acc.pending > 0 => BusResponse::Stall(0),
            Region::Ram => {
                let now = self.native_clock.native_ticks() + acc.pending;
                let waits = waits
                    + if now >= self.next_refresh {
                        self.ram_refresh(now)
                    } else {
                        0
                    };
                bus_access_long(acc, waits, self, Self::ram_read, Self::ram_write)
            }
            Region::Rom => bus_access_long(acc, waits, self, Self::rom_read, Self::ignore_write),
            Region::Io => self.io_cycle(acc),
            Region::CpuId => bus_access_long(acc, 0, self, Self::cpu_id_read, Self::ignore_write),
            Region::Video => bus_access_long(acc, waits, self, Self::video_read, Self::video_write),
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
        }

        // Keep the RTC and ADB for PRAM and event channels
        let Via { adb, rtc, .. } = std::mem::replace(&mut self.via1, Via::new(self.model));
        self.via1.adb = adb;
        self.via1.rtc = rtc;

        self.swim.reset();
        self.scc = Scc::new(SCC_RTXC_FREQUENCY);
        self.via2 = Via2::new(self.model);
        self.dafb.reset();
        self.sound.reset();
        self.scsi.reset();
        self.scsi_dma_stalled = false;
        self.scsi_dma_done = 0;
        self.iosb_regs.fill(0);
        self.memc_regs = MEMC_RESET;
        self.vram_row = 0;
        self.ram_map_update();

        self.mouse_ready = false;
        self.overlay = true;
        self.sync_via1_pa();
        Ok(false)
    }

    fn localtalk_poll_needed(&self) -> bool {
        self.scc.localtalk_poll_needed(SccCh::B)
    }
}

impl<TRenderer> Tickable for Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
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

                self.tick60_clock += 1;
                if self.tick60_clock >= TICK60_PERIOD {
                    self.tick60_clock = 0;
                    self.via1.set_line(ViaLine::CA1, LineState::Pulse);
                }
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

        // Sound. The interrupt output is active low on VIA 2 CB1.
        self.via2.set_line(ViaLine::CB1, (!self.sound.irq()).into());

        let c16m_ticks = self.native_clock.c16m_ticks() - c16m_start;
        self.sound_clock += c16m_ticks;
        while self.sound_clock >= self.sound.sample_period() {
            self.sound_clock -= self.sound.sample_period();
            self.sound
                .tick(self.speed == EmulatorSpeed::Accurate, 0.0)?;
        }

        self.dafb.tick(native_ticks, ctx)?;
        let video_irq = self.dafb.get_irq();
        self.via2.a_in.set_v2irqvideo(!video_irq);

        self.scsi.tick(native_ticks, ctx)?;

        // The flags of the slots and of the 53C96 latch the falling edge of
        // their active low line, as on the LC 475
        self.via2.set_line(ViaLine::CA1, (!video_irq).into());
        self.via2
            .set_line(ViaLine::CB2, (!self.scsi.get_irq()).into());

        // The flag of DREQ follows the level of its line: the ROM polls it in
        // the IFR to know when the 53C96 wants a pseudo-DMA access
        if self.scsi.get_drq() {
            self.via2.set_irq(IRQ_CA2);
        } else {
            self.via2.clear_irq(IRQ_CA2);
        }

        // The SWIM is on the I/O bus, clocked by C16M
        self.swim.tick(c16m_ticks, ())?;

        Ok(1)
    }
}

impl<TRenderer> Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn compute_irq(&mut self) -> Option<u8> {
        // The Quadra 700 has a second set of interrupt levels for A/UX, selected
        // by VIA 1 PB6 (its Developer Note, table 2-7). IOSB has only PB0-PB5
        // on its pins (schematic), and it isn't known whether it has such
        // levels, how they would be selected, or whether any system uses them
        // on this machine. A/UX runs with the Macintosh levels here.
        if self.progkey_pressed.get_clear() {
            return Some(7);
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

impl<TRenderer> IrqSource for Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
        if let Some(irq) = self.irq {
            return irq;
        }
        let irq = self.compute_irq();
        self.irq = Some(irq);
        irq
    }
}

impl<TRenderer> InspectableBus<Address, Byte> for Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
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
        const BLOCK: Address = 1 << RAM_BLOCK_SHIFT;
        let offset = match MAP[usize::from(self.overlay)][(addr >> 24) as usize] {
            Region::Ram => self.ram_index(addr).filter(|&idx| idx < self.ram.len()),
            _ => None,
        };
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
        self.ram_map
            .iter()
            .fold(u32::from(self.overlay), |h, &block| {
                h.wrapping_mul(31).wrapping_add(block)
            })
    }
}

impl<TRenderer> Debuggable for Quadra800Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_group, dbgprop_nest};

        let mut result = vec![
            dbgprop_nest!("Apple Desktop Bus", self.via1.adb),
            dbgprop_nest!("Sound", self.sound),
            dbgprop_nest!("Built-in video (djMEMC)", self.dafb),
            dbgprop_nest!("SCSI controller (NCR 53C96)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA 1 (SY6522)", self.via1),
            dbgprop_nest!("VIA 2 (SY6522)", self.via2),
        ];

        for slot in self.model.nubus_slots() {
            result.push(dbgprop_group!(
                format!("NuBus slot ${:1X} (empty)", slot),
                vec![]
            ));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    fn bus(ram_size: usize) -> Quadra800Bus<NullRenderer> {
        Quadra800Bus::new(
            MacModel::Quadra800,
            &vec![0; 1024 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MacMonitor::default(),
            MouseMode::Disabled,
            Some(ram_size),
            None,
        )
    }

    const MB: usize = 1024 * 1024;

    #[test]
    fn ram_bank_sizes() {
        let sizes = |ram| Quadra800Bus::<NullRenderer>::ram_bank_sizes(ram * MB);
        assert_eq!(sizes(8), [4 * MB, 4 * MB, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(sizes(16)[2..5], [4 * MB, 4 * MB, 0]);
        assert_eq!(sizes(24)[2..4], [16 * MB, 0]);
        assert_eq!(sizes(40)[2..5], [16 * MB, 16 * MB, 0]);
        assert_eq!(sizes(136)[2..], [16 * MB; 8]);
    }

    /// The ROM sizes each bank at its power up base, then moves them together
    #[test]
    fn ram_banks() {
        let mut bus = bus(40 * MB);
        assert_eq!(bus.ram_index(0x003F_FFFF), Some(0x3F_FFFF));
        assert_eq!(bus.ram_index(0x0040_0000), None);
        assert_eq!(bus.ram_index(0x0400_0000), Some(0x40_0000));
        assert_eq!(bus.ram_index(0x0800_0000), Some(0x80_0000));
        assert_eq!(bus.ram_index(0x08FF_FFFF), Some(0x17F_FFFF));
        assert_eq!(bus.ram_index(0x0900_0000), None);
        assert_eq!(bus.ram_index(0x0C00_0000), Some(0x180_0000));

        // Bases in 4 MB units, written as longs: the board, then the two SIMM
        // banks, then the empty ones at the top
        let bases = [0, 1, 2, 6, 10, 10, 10, 10, 10, 10];
        for (bank, base) in bases.into_iter().enumerate() {
            bus.io_write(0x5000_E004 + 4 * bank as Address, 4, 0x100 | base);
        }
        assert_eq!(bus.ram_index(0x0040_0000), Some(0x40_0000));
        assert_eq!(bus.ram_index(0x0080_0000), Some(0x80_0000));
        assert_eq!(bus.ram_index(0x027F_FFFF), Some(0x27F_FFFF));
        assert_eq!(bus.ram_index(0x0280_0000), None);
        assert_eq!(bus.io_read(0x5000_E00C, 4), Some(0x102));
    }

    #[test]
    fn memc_registers() {
        let mut bus = bus(8 * MB);
        // 12 bits, in the low bits of the long
        bus.io_write(0x5000_E034, 4, 0xFFFF_F1E7);
        assert_eq!(bus.io_read(0x5000_E034, 4), Some(0x1E7));
        // The version is read only
        bus.io_write(0x5000_E000, 4, 0xFFF);
        assert_eq!(bus.io_read(0x5000_E000, 4), Some(0x1FF));
    }

    #[test]
    fn machine_id() {
        let mut bus = bus(8 * MB);
        assert_eq!(bus.cpu_id_read(CPU_ID_ADDR, 4), Some(0xA55A_2BAD));
        // VIA 1 ID pins PA6, PA4, PA2, PA1: the Quadra 800
        assert_eq!(bus.io_read_byte(0x5000_1E00).unwrap() & 0x56, 0x12);
    }

    #[test]
    fn empty_slots_are_a_bus_error() {
        for block in [0x60, 0x90, 0xC0, 0xE0, 0xFC, 0xFD, 0xFE] {
            assert!(matches!(MAP[0][block], Region::None));
        }
        assert!(matches!(MAP[0][0xF9], Region::Video));
    }
}
