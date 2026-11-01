//! Macintosh LC 475: PrimeTime and MEMCjr
//!
//! PrimeTime holds VIA 1, VIA 2 (a real VIA core without timers or shift
//! register), the SWIM II and the sound, decodes the I/O area and bridges the
//! CPU bus to the I/O bus, where the 53C96 and the SCC are. MEMCjr holds the
//! memory timing and the video, which is DAFB compatible. A Cuda has ADB, the
//! clock and PRAM.
//!
//! IOSB (Quadra 800, Centris 650, 610) looks very close to PrimeTime, and
//! PrimeTime II (LC 630, Quadra 630) is more or less PrimeTime with ATA, so this
//! is where those are expected to start from. Only PrimeTime is here for now.

use std::thread;
use std::time::{Duration, Instant};

use crate::bus::access::{Bus68040, BusAccess, BusResponse, bus_access_long};
use crate::bus::native_clock::{C16M_FREQUENCY, DeviceCycle, NativeClock};
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::cuda::Cuda;
use crate::mac::macii::via2::Via2;
use crate::mac::maclc::via::Via;
use crate::mac::quadra::dafb::{Dafb, DafbKind};
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::esp::Esp;
use crate::mac::sound::dfac2::Dfac2;
use crate::mac::sound::v8_sonora::{V8Sound, V8Variant};
use crate::mac::swim::Swim;
use crate::mac::via_common::{IRQ_CA2, LineState, ViaLine};
use crate::mac::{MacModel, MacMonitor};
use crate::renderer::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, Renderer, null_audio_sink,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};
use serde::{Deserialize, Serialize};

/// Frequency of the bus of the real hardware, without overclocking: the
/// 25 MHz of the 68LC040
const BUS_FREQUENCY: Ticks = 25_000_000;

/// Long at the end of the I/O space which identifies the machine. The low bits
/// are straps: bits 1-0 for the clock (01 = 25 MHz), bit 2 is J18 (open =
/// Primus, the LC 475; a jumper makes it an Aladdin, the Quadra 605).
const CPU_ID: u32 = 0xA55A_2221;
const CPU_ID_ADDR: Address = 0x5FFF_FFFC;

/// Period of the 60.15 Hz interrupt of PrimeTime on VIA 1 CA1, in VIA ticks
const TICK60_PERIOD: Ticks = 13024;

/// Wait states, in C16M ticks, of an access to a device on the I/O bus, on top
/// of the bus cycle itself: PrimeTime synchronises the cycle with the I/O bus
/// clock and its termination back with the CPU clock. First fit to BusTiming on
/// a 25 MHz Quadra 610 (IOSB, as no stock LC 475 has been measured): VIA 2 and
/// the ISM read in 12 clocks, the 53C96 in 12 and the ASC in 13.
const IO_BUS_WAITS: Ticks = 5;

/// Extra wait states, in C16M ticks, of an SCC access, for PCLK: the Quadra 610
/// reads it in 17 clocks. The recovery time between accesses isn't modelled.
const SCC_PCLK_WAITS: Ticks = 3;

/// C16M ticks from the VIA 1 tick that an access starts on to the CPU's release:
/// BusTiming holds the CPU 0.76 VIA cycle on a Quadra 610, 0.70 on the LC 475 at
/// 33 MHz
const VIA_RELEASE: Ticks = 15;

/// Clock of the RTxCA and RTxCB pins of the SCC, 3.6864 MHz, which the baud
/// rate generators run from
const SCC_RTXC_FREQUENCY: Ticks = 3_686_400;

/// Sizes of video RAM which can be fitted
pub const VRAM_SIZES: &[usize] = &[512 * 1024, 1024 * 1024];

/// Bytes of the PrimeTime registers kept
const PRIMETIME_REGS_SIZE: usize = 0x80;

/// RAM soldered on the board, which is bank 0. A SIMM is bank 1.
const BOARD_RAM: usize = 4 * 1024 * 1024;

/// Banks of RAM that MEMCjr decodes, and where each starts at power up: one
/// every 64 MB. The ROM sizes each bank there, one size that is a power of two
/// per bank, then writes the base addresses that make the RAM contiguous.
const RAM_BANKS: usize = 5;
const RAM_BANK_RESET: u16 = 64;

/// Registers of MEMCjr, one every 4 bytes, and the one of RAM bank 0
const MEMCJR_REGS: usize = 16;
const MEMCJR_BANK0: usize = 1;
/// Their values at power up
const MEMCJR_RESET: [u16; MEMCJR_REGS] = [
    // Version and features
    0x420,
    // RAM banks
    0,
    RAM_BANK_RESET,
    2 * RAM_BANK_RESET,
    3 * RAM_BANK_RESET,
    4 * RAM_BANK_RESET,
    // RAM banks 5 to 9, which don't exist
    0xFFF,
    0xFFF,
    0xFFF,
    0xFFF,
    0xFFF,
    // Top of the RAM, in MB
    0x3FF,
    // Configuration
    0x0DF,
    // Refresh interval
    129,
    // Test
    0,
    // Current refresh count, not modelled
    0,
];

/// RAM refresh: every 15.6 us, keeping the RAM busy for 6 clocks
const RAM_REFRESH_PERIOD: Ticks = 390;
const RAM_REFRESH_WAITS: Ticks = 6;

/// RAM decode granularity, and number of such blocks in the RAM space
const RAM_BLOCK_SHIFT: u32 = 20;
const RAM_BLOCKS: usize = 1024;

/// Marks a RAM decode block with nothing in it
const RAM_BLOCK_NONE: u32 = u32::MAX;

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

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
    /// Expansion slot, as NuBus super slot $E and slot $E
    Slot,
    /// Built-in video (MEMCjr), slot $9
    Video,
}

const fn region(overlay: bool, block: usize) -> Region {
    match block {
        0x00..=0x3F if overlay => Region::Rom,
        0x00..=0x3F => Region::Ram,
        0x40..=0x4F => Region::Rom,
        0x50..=0x53 => Region::Io,
        0x5F => Region::CpuId,
        0xE0..=0xEF | 0xFE => Region::Slot,
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
pub struct Lc475Bus<TRenderer: Renderer> {
    /// The currently emulated Macintosh model
    model: MacModel,

    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via1: Via,
    pub(crate) via2: Via2,
    pub(crate) cuda: Cuda,
    /// The only device on the IIC bus of the Cuda (schematic)
    pub(crate) dfac2: Dfac2,
    pub(crate) scc: Scc,
    pub(crate) sound: V8Sound,
    /// C16M ticks into the current sound sample
    sound_clock: Ticks,
    /// VIA ticks into the 60.15 Hz period
    tick60_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: Esp,
    /// A pseudo-DMA access is held off, waiting for DREQ
    scsi_dma_stalled: bool,
    /// Halves of a pseudo-DMA long already moved, while the access is held off
    scsi_dma_done: u32,
    /// Data of a pseudo-DMA read
    scsi_dma_result: u32,
    /// Registers of PrimeTime, which are only stored: they set up the timing of
    /// the 53C96 accesses
    primetime_regs: Vec<u8>,
    /// Upper 6 bits of a 12-bit register, which MEMCjr moves 6 bits at a time:
    /// those of its own registers and of the video
    memcjr_holding: u8,
    /// Registers of MEMCjr, only stored apart from the base of the RAM banks,
    /// in MB in bits 9-0. Bit 10 of a bank is its DRAM addressing mode, which
    /// the ROM sets on a bank whose size isn't a power of two. All memory has
    /// a single mode here, and the timing doesn't follow the configuration.
    memcjr_regs: [u16; MEMCJR_REGS],
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

impl<TRenderer> Lc475Bus<TRenderer>
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

    /// MTemp address, Y coordinate (16 bit, signed)
    const ADDR_MTEMP_Y: Address = 0x0828;
    /// MTemp address, X coordinate (16 bit, signed)
    const ADDR_MTEMP_X: Address = 0x082A;
    /// RawMouse address, Y coordinate (16 bit, signed)
    const ADDR_RAWMOUSE_Y: Address = 0x082C;
    /// RawMouse address, Y coordinate (16 bit, signed)
    const ADDR_RAWMOUSE_X: Address = 0x082E;
    /// CrsrNew address
    const ADDR_CRSRNEW: Address = 0x08CE;

    #[allow(clippy::too_many_arguments)]
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

        let mut cuda = Cuda::default();
        cuda.adb_add_device(AdbMouse::new());
        cuda.adb_add_device(AdbKeyboard::new());

        let mut bus = Self {
            model,

            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            via1: Via::new(model),
            via2: Self::new_via2(model),
            cuda,
            dfac2: Dfac2::default(),
            scc: Scc::new(SCC_RTXC_FREQUENCY),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), C16M_FREQUENCY),
            // MODE pins strapped for a 16-bit DMA bus, as on the Quadra 700
            scsi: Esp::new(true),
            scsi_dma_stalled: false,
            scsi_dma_done: 0,
            scsi_dma_result: 0,
            primetime_regs: vec![0; PRIMETIME_REGS_SIZE],
            memcjr_holding: 0,
            memcjr_regs: MEMCJR_RESET,
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
        bus.ram_map_update();
        bus
    }

    /// First MB of a bank in the RAM space
    fn ram_bank_base(&self, bank: usize) -> usize {
        usize::from(self.memcjr_regs[MEMCJR_BANK0 + bank] & 0x3FF)
    }

    /// Size and offset in `ram` of a bank
    fn ram_bank(&self, bank: usize) -> (usize, usize) {
        match bank {
            0 => (self.ram.len().min(BOARD_RAM), 0),
            1 => (self.ram.len().saturating_sub(BOARD_RAM), BOARD_RAM),
            _ => (0, 0),
        }
    }

    /// Rebuilds the decode of the RAM space. An address belongs to the last
    /// bank which starts at or below it, and past the size of the bank nothing
    /// answers.
    fn ram_map_update(&mut self) {
        for block in 0..RAM_BLOCKS {
            let bank = (0..RAM_BANKS)
                .rev()
                .find(|&b| self.ram_bank_base(b) <= block);
            self.ram_map[block] = bank.map_or(RAM_BLOCK_NONE, |bank| {
                let (size, offset) = self.ram_bank(bank);
                let inside = (block - self.ram_bank_base(bank)) << RAM_BLOCK_SHIFT;
                if inside < size {
                    (offset + inside) as u32
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

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / crate::mac::compact::bus::RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.cuda.rtc().effective_speed()
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

    #[allow(clippy::needless_pass_by_value)]
    fn write_ram<T: ToBytes>(&mut self, addr: Address, val: T) {
        let addr = addr as usize;
        let bytes = val.to_be_bytes();
        for (i, &b) in bytes.as_ref().iter().enumerate() {
            self.ram[addr + i] = b;
            self.ram_dirty.insert((addr + i) / RAM_DIRTY_PAGESIZE);
        }
    }

    fn read_ram<T: PrimInt + FromPrimitive>(&self, addr: Address) -> T {
        let addr = addr as usize;
        let len = std::mem::size_of::<T>();
        let end = addr + len;

        assert!(len <= 4);
        let mut tmp = [0_u8; 4];
        tmp[4 - len..].copy_from_slice(&self.ram[addr..end]);
        T::from_u32(u32::from_be_bytes(tmp)).unwrap()
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
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
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

    /// The registers below the RAMDAC are 12 bits wide and MEMCjr has 6 bits
    /// to them: the upper ones go through a holding register in its own
    /// registers.
    const fn video_held(addr: Address) -> bool {
        (addr & 0xFF_FFFF) >= 0x80_0000 && (addr & 0xFF_FFFF) < 0x80_0200
    }

    /// The holding register also answers among the video registers
    const fn video_holding(addr: Address) -> bool {
        addr & 0xFF_FEFF == 0x80_007C
    }

    fn video_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        if Self::video_holding(addr) && len == 4 {
            return Some(u32::from(self.memcjr_holding));
        }
        let val = (addr..addr + len).try_fold(0, |v, a| {
            Some((v << 8) | u32::from(self.dafb.read(a & 0xFF_FFFF)?))
        })?;
        if Self::video_held(addr) && len == 4 {
            self.memcjr_holding = ((val >> 6) & 0x3F) as u8;
            return Some(val & 0x3F);
        }
        Some(val)
    }

    fn video_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        if Self::video_holding(addr) && len == 4 {
            self.memcjr_holding = (val & 0x3F) as u8;
            return Some(());
        }
        let val = if Self::video_held(addr) && len == 4 {
            (val & 0x3F) | (u32::from(std::mem::take(&mut self.memcjr_holding)) << 6)
        } else {
            val
        };
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
            // MEMCjr: it drives 6 bits, and a read of a register moves the
            // upper bits to the holding register
            0x0000_E07F => Some(self.memcjr_holding),
            reg @ 0x0000_E000..=0x0000_E03F if reg & 3 == 3 => {
                let reg = self.memcjr_regs[Self::memcjr_reg(addr)];
                self.memcjr_holding = ((reg >> 6) & 0x3F) as u8;
                Some((reg & 0x3F) as u8)
            }
            0x0000_E000..=0x0000_FFFF => Some(0),
            // SCSI (NCR 53C96)
            0x0001_0000..=0x0001_00FF => Some(Self::dev_read(addr, self.scsi.read(addr))),
            // Sound
            0x0001_4000..=0x0001_4FFF => Some(Self::dev_read(addr, self.sound.read(addr & 0xFFF))),
            // PrimeTime registers
            0x0001_8000..=0x0001_9FFF => Some(self.primetime_regs[Self::primetime_reg(addr)]),
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
            // MEMCjr
            0x0000_E07F => {
                self.memcjr_holding = val & 0x3F;
                Some(())
            }
            reg @ 0x0000_E000..=0x0000_E03F if reg & 3 == 3 => {
                self.memcjr_write(Self::memcjr_reg(addr), val);
                Some(())
            }
            0x0000_E000..=0x0000_FFFF => Some(()),
            // SCSI (NCR 53C96)
            0x0001_0000..=0x0001_00FF => Self::dev_write(addr, val, self.scsi.write(addr, val)),
            // Sound
            0x0001_4000..=0x0001_4FFF => {
                Self::dev_write(addr, val, self.sound.write(addr & 0xFFF, val))
            }
            // PrimeTime registers
            0x0001_8000..=0x0001_9FFF => {
                self.primetime_regs[Self::primetime_reg(addr)] = val;
                Some(())
            }
            // SWIM
            0x0001_E000..=0x0001_FFFF => Self::dev_write(addr, val, self.swim.write(addr, val)),
            _ => None,
        }
    }

    /// Number of a MEMCjr register
    fn memcjr_reg(addr: Address) -> usize {
        ((addr & Self::IO_MASK) as usize - 0xE000) / 4
    }

    /// A write of a MEMCjr register, whose upper bits come from the holding
    /// register
    fn memcjr_write(&mut self, reg: usize, val: Byte) {
        let holding = u16::from(std::mem::take(&mut self.memcjr_holding));
        let val = (holding << 6) | u16::from(val & 0x3F);
        match reg {
            // Bank 0 always starts at 0
            MEMCJR_BANK0 => self.memcjr_regs[reg] = val & !0x3FF,
            // The other RAM banks
            2..=5 => {
                self.memcjr_regs[reg] = val;
                self.ram_map_update();
            }
            // Read only
            0 | 6..=10 | 15 => (),
            _ => self.memcjr_regs[reg] = val,
        }
    }

    /// Index of a PrimeTime register byte: the registers are 16 bits wide, one
    /// every $100 bytes
    fn primetime_reg(addr: Address) -> usize {
        ((addr as usize >> 7) & !1 | (addr as usize & 1)) % PRIMETIME_REGS_SIZE
    }

    /// An access to the I/O area. PrimeTime completes all the cycles here, also
    /// to the 53C96 and the SCC, which it bridges to the I/O bus. MEMCjr is on
    /// the CPU bus.
    fn io_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        match acc.addr & Self::IO_MASK {
            // VIA 1. VIA 2 has no timers, and doesn't take the VIA clock either:
            // its reads take the same time at any spacing.
            0x0000_0000..=0x0000_1FFF => self.via_cycle(acc),
            0x0000_2000..=0x0000_3FFF => self.io_bus_cycle(acc, 0, false),
            // SCC
            0x0000_C000..=0x0000_DFFF => self.io_bus_cycle(acc, SCC_PCLK_WAITS, false),
            // 53C96 pseudo-DMA
            0x0001_0100..=0x0001_0103 => self.scsi_dma_cycle(acc),
            // 53C96, sound, PrimeTime registers
            0x0001_0000..=0x0001_00FF | 0x0001_4000..=0x0001_4FFF | 0x0001_8000..=0x0001_9FFF => {
                self.io_bus_cycle(acc, 0, false)
            }
            // SWIM
            0x0001_E000..=0x0001_FFFF => self.io_bus_cycle(acc, 0, true),
            // MEMCjr, and nothing
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
    /// released part way through the VIA cycle. Real LC 475: one VIA cycle back
    /// to back.
    fn via_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        match self.native_clock.via_sync(&acc, VIA_RELEASE) {
            DeviceCycle::Stall(n) => BusResponse::Stall(n),
            DeviceCycle::Access(w) => bus_access_long(acc, w, self, Self::io_read, Self::io_write),
        }
    }

    /// An access to a device on the I/O bus: it starts on a C16M tick and
    /// lasts a bus cycle in C16M ticks, plus the IO_BUS_WAITS of PrimeTime
    /// and the device's own `waits`, in C16M ticks. When
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

    /// An access to the expansion slot, on the I/O bus: the slot isn't
    /// connected to the CPU (Developer Note), and its cards are at most 16 bits
    /// wide unless they say otherwise (16MASTER is for bus masters). PrimeTime
    /// makes up to two cycles of a long word.
    fn slot_cycle(&mut self, acc: BusAccess<Bus68040>) -> BusResponse {
        let w = match self.native_clock.device16_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        let w = self.native_clock.scale_dev16_waits(w, IO_BUS_WAITS);
        bus_access_long(acc, w, self, Self::slot_read, Self::slot_write)
    }

    fn slot_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let mut val = 0;
        let mut a = addr;
        while a < addr + len {
            // Up to the next 16-bit boundary or the end of the access
            let step = (2 - (a & 1)).min(addr + len - a);
            val = (val << (8 * step)) | self.slot_port_read(a & 0xFF_FFFF, step)?;
            a += step;
        }
        Some(val)
    }

    fn slot_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        let mut a = addr;
        while a < addr + len {
            let step = (2 - (a & 1)).min(addr + len - a);
            let shift = 8 * (addr + len - a - step);
            let part = (val >> shift) & ((1 << (8 * step)) - 1);
            self.slot_port_write(a & 0xFF_FFFF, step, part)?;
            a += step;
        }
        Some(())
    }

    /// One cycle of a card, with the offset in the slot: without a card
    /// nothing answers, which is a bus error, as the Slot Manager expects.
    #[allow(clippy::needless_pass_by_ref_mut)]
    fn slot_port_read(&mut self, _offset: Address, _len: u32) -> Option<u32> {
        None
    }

    #[allow(clippy::needless_pass_by_ref_mut)]
    fn slot_port_write(&mut self, _offset: Address, _len: u32, _val: u32) -> Option<()> {
        None
    }

    /// SLOTE_IRQ from a card (schematic: its own pin on PrimeTime)
    ///
    /// The slot interrupts are VIA 2 port A inputs, active low, summed up in
    /// the CA1 flag, which raises level 2. The ROM's handler ($4088BC56) clears
    /// that flag, then scans bits 0-6, and its table ($40806F14) gives the slots:
    /// bits 0-5 are slots $9-$E and bit 6 is slot 0, the built-in video. Only bits
    /// 5 and 6 exist on this board. A machine with NuBus would wire its slots to
    /// the bits below 5 the same way, each also feeding CA1. The Developer Note
    /// confirms bit 5 for /SLOTIRQ. Nothing interrupts through this yet, so
    /// whether CA1 follows the line or latches is untested on the real thing.
    fn slot_irq(&self) -> bool {
        false
    }

    /// A pseudo-DMA access to the 53C96. PrimeTime holds the cycle off until
    /// DREQ, then makes one cycle of the 16-bit DMA bus for a word, on D31-D16,
    /// and two for a long, high half first (MAME; no real hardware data). A
    /// long is one bus transfer for the 68040, so a hold-off between the halves
    /// is PrimeTime's too: when the second half isn't there yet, the first was
    /// already moved and only the second is waited for.
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
                error!(
                    "ESP CHECK: pseudo-DMA {} at {:08X} size {} timed out: {}",
                    if acc.write { "write" } else { "read" },
                    acc.addr,
                    acc.size,
                    self.scsi.dbg()
                );
                return BusResponse::BusError;
            }
            self.scsi_dma_stalled = false;
            self.scsi_dma_transfer(acc.write, acc.data, acc.size);
            self.scsi_dma_done += 1;
        }
        self.scsi_dma_done = 0;
        bus_access_long(acc, w, self, Self::scsi_dma_read, Self::ignore_write)
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
                half << 16
            };
        }
    }

    fn scsi_dma_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let shift = 8 * (4 - (addr & 3) - len);
        let mask = if len == 4 { !0 } else { (1 << (8 * len)) - 1 };
        Some((self.scsi_dma_result >> shift) & mask)
    }

    /// VIA 2 at power-up. PrimeTime has no VIA ID pins on it, and its timers
    /// don't run: this bus doesn't tick them.
    fn new_via2(model: MacModel) -> Via2 {
        Via2::new(model)
    }

    /// Syncs the devices driven by the VIA1 port A pins
    fn sync_via1_pa(&mut self) {
        self.swim.sel = self.via1.pa().headsel();
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        if self.mouse_mode == MouseMode::Disabled {
            return;
        }

        if button.is_some() {
            self.cuda.adb_event(&AdbEvent::Mouse(MouseEvent {
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
                self.cuda.adb_event(&AdbEvent::Mouse(MouseEvent {
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

    /// Updates the mouse position (absolute coordinates)
    pub fn mouse_update_abs(&mut self, x: u16, y: u16) {
        if self.mouse_mode != MouseMode::Absolute {
            return;
        }

        let old_x = self.read_ram::<u16>(Self::ADDR_RAWMOUSE_X);
        let old_y = self.read_ram::<u16>(Self::ADDR_RAWMOUSE_Y);

        if !self.mouse_ready && (old_x != 15 || old_y != 15) {
            // Wait until the boot process has initialized the mouse position so we don't
            // interfere with the memory test.
            return;
        }
        self.mouse_ready = true;

        // Report updated mouse coordinates to OS. The current (RawMouse) and
        // new (MTemp) positions are the same: as on the LC III, the ROM takes
        // any difference between them for a relative movement.
        self.write_ram(Self::ADDR_MTEMP_X, x);
        self.write_ram(Self::ADDR_MTEMP_Y, y);
        self.write_ram(Self::ADDR_RAWMOUSE_X, x);
        self.write_ram(Self::ADDR_RAWMOUSE_Y, y);
        self.write_ram(Self::ADDR_CRSRNEW, 1_u8);
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
        self.cuda.adb_event(&AdbEvent::Key(ke));
    }

    /// Releases all pressed inputs
    pub fn input_release_all(&mut self) {
        self.cuda.adb_event(&AdbEvent::ReleaseAll);
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        self.cuda.rtc_mut()
    }
}

impl<TRenderer> Lc475Bus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Wait states of the memories on the CPU bus (RAM and ROM, VRAM behind
    /// MEMCjr): for a single cycle or the first long word of a burst, and for
    /// each further long word of a burst. None for the other targets, which
    /// don't take bursts. These are the times for 25 MHz: RAM 5-2-2-2 reading and
    /// 4-2-2-2 writing, ROM 5, the screen in its current row 4-3-3-3 and
    /// 3-2-2-2, which are the same at 33 MHz, as measured. Real writes are
    /// posted, which the bus can't express: only their period is right.
    fn memory_waits(region: Region, addr: Address, write: bool) -> Option<(Ticks, Ticks)> {
        match region {
            Region::Ram => Some((3, 1)),
            Region::Rom => Some((3, 3)),
            Region::Video if (addr & 0xFF_FFFF) < 0x20_0000 => Some((2, if write { 1 } else { 2 })),
            _ => None,
        }
    }
}

impl<TRenderer> Lc475Bus<TRenderer>
where
    TRenderer: Renderer,
{
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

impl<TRenderer> Bus<Address, Byte> for Lc475Bus<TRenderer>
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
            Region::Slot => self.slot_cycle(acc),
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
        if hard {
            // Clear RAM
            self.ram.fill(0);
            self.ram_dirty
                .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
            // The Cuda also goes through its start
            self.cuda.reset();
            self.dfac2.reset();
        }

        self.via1.reset();
        self.via1 = Via::new(self.model);
        self.scc = Scc::new(SCC_RTXC_FREQUENCY);
        self.via2 = Self::new_via2(self.model);
        self.dafb.reset();
        self.sound.reset();
        self.scsi.reset();
        self.scsi_dma_stalled = false;
        self.scsi_dma_done = 0;
        self.primetime_regs.fill(0);
        self.memcjr_holding = 0;
        self.memcjr_regs = MEMCJR_RESET;
        self.vram_row = 0;
        self.ram_map_update();

        self.mouse_ready = false;
        self.overlay = true;
        self.sync_via1_pa();
        Ok(false)
    }

    fn take_reset_request(&mut self) -> bool {
        self.cuda.take_reset_request()
    }
}

impl<TRenderer> Tickable for Lc475Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks, _: ()) -> Result<Ticks> {
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
                self.via1.tick(1)?;

                let pb = self.via1.pb();
                let via = &mut self.via1;
                // TIP is on PB5, TACK on PB4, both active low
                self.cuda
                    .tick(1, !pb.sys_ses(), !pb.via_full(), &mut self.dfac2, |b| {
                        via.shift_sr(b)
                    });
                self.via1.b_in.set_xcvr_ses(!self.cuda.treq());

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
            self.sound.tick(self.speed == EmulatorSpeed::Accurate)?;
        }

        self.dafb.tick(native_ticks, ctx)?;
        let video_irq = self.dafb.get_irq();
        let slot_irq = self.slot_irq();
        self.via2.a_in.set_v2irqvideo(!video_irq);
        self.via2.a_in.set_v2irq6(!slot_irq);

        self.scsi.tick(native_ticks, ctx)?;

        // The flags of the slots and of the 53C96 latch the falling edge of
        // their active low line, as measured on a real LC 475: an IFR write
        // drops them while the line is still low, and the flag of the 53C96
        // stays set after its interrupt drops (MAME has them follow the line).
        self.via2
            .set_line(ViaLine::CA1, (!(video_irq || slot_irq)).into());
        self.via2
            .set_line(ViaLine::CB2, (!self.scsi.get_irq()).into());

        // The flag of DREQ follows the level of its line: the ROM polls it in
        // the IFR to know when the 53C96 wants a pseudo-DMA access. Not
        // measured.
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

impl<TRenderer> IrqSource for Lc475Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
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

impl<TRenderer> InspectableBus<Address, Byte> for Lc475Bus<TRenderer>
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
}

impl<TRenderer> Debuggable for Lc475Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        vec![
            dbgprop_nest!("Cuda", self.cuda),
            dbgprop_nest!("DFAC II", self.dfac2),
            dbgprop_nest!("Sound", self.sound),
            dbgprop_nest!("Built-in video (MEMCjr)", self.dafb),
            dbgprop_nest!("SCSI controller (NCR 53C96)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA 1 (SY6522)", self.via1),
            dbgprop_nest!("VIA 2 (SY6522)", self.via2),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    fn bus() -> Lc475Bus<NullRenderer> {
        Lc475Bus::new(
            MacModel::LC475,
            &vec![0; 1024 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MacMonitor::default(),
            MouseMode::Disabled,
            Some(4 * 1024 * 1024),
            None,
        )
    }

    #[test]
    fn empty_slot_is_a_bus_error() {
        let mut bus = bus();
        assert_eq!(bus.slot_read(0xFEFF_FFFC, 4), None);
        assert_eq!(bus.slot_write(0xFEE0_0000, 2, 0), None);
        assert!(matches!(MAP[0][0xFE], Region::Slot));
        assert!(matches!(MAP[0][0xE0], Region::Slot));
        assert!(matches!(MAP[0][0xFD], Region::None));
    }

    /// The ROM sizes one bank that's a power of two at a time, at the power
    /// up bases, then moves them together
    #[test]
    fn ram_banks() {
        let mut bus = Lc475Bus::new(
            MacModel::LC475,
            &vec![0; 1024 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MacMonitor::default(),
            MouseMode::Disabled,
            Some(12 * 1024 * 1024),
            None,
        );
        assert_eq!(bus.ram_index(0x003F_FFFF), Some(0x3F_FFFF));
        assert_eq!(bus.ram_index(0x0040_0000), None);
        assert_eq!(bus.ram_index(0x0400_0000), Some(0x40_0000));
        assert_eq!(bus.ram_index(0x047F_FFFF), Some(0xBF_FFFF));
        assert_eq!(bus.ram_index(0x0480_0000), None);

        // Bank 1 at 4 MB, through the holding register. The others at the top.
        bus.io_write_byte(0x5000_E07F, 0);
        bus.io_write_byte(0x5000_E00B, 4);
        for reg in [0xE00F, 0xE013, 0xE017] {
            bus.io_write_byte(0x5000_E07F, 0);
            bus.io_write_byte(0x5000_0000 + reg, 12);
        }
        assert_eq!(bus.ram_index(0x0040_0000), Some(0x40_0000));
        assert_eq!(bus.ram_index(0x00BF_FFFF), Some(0xBF_FFFF));
        assert_eq!(bus.ram_index(0x00C0_0000), None);
    }

    #[test]
    fn memcjr_registers() {
        let mut bus = bus();
        // Refresh interval, through the holding register, also at the video
        bus.io_write_byte(0x5000_E07F, 0x05);
        bus.io_write_byte(0x5000_E037, 0x2B);
        assert_eq!(bus.io_read_byte(0x5000_E037), Some(0x2B));
        assert_eq!(bus.video_read(0xF980_007C, 4), Some(0x05));
        assert_eq!(bus.video_read(0xF980_017C, 4), Some(0x05));

        // Read only: the version, a bank that doesn't exist
        for reg in [0xE003, 0xE01B] {
            let old = bus.io_read_byte(0x5000_0000 + reg);
            bus.io_write_byte(0x5000_0000 + reg, 0x11);
            assert_eq!(bus.io_read_byte(0x5000_0000 + reg), old);
        }
        assert_eq!(bus.io_read_byte(0x5000_E01B), Some(0x3F));

        // Bank 0 keeps its base and takes the addressing mode
        bus.io_write_byte(0x5000_E07F, 0x10);
        bus.io_write_byte(0x5000_E007, 0x3F);
        assert_eq!(bus.io_read_byte(0x5000_E007), Some(0));
        assert_eq!(bus.io_read_byte(0x5000_E07F), Some(0x10));
    }
}
