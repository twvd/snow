use std::thread;
use std::time::{Duration, Instant};

use super::via2::Via2;
use crate::bus::access::{Bus68020, BusAccess, BusResponse, bus_access_byte, bus_access_long};
use crate::bus::native_clock::{C16M_PER_VIA, DeviceCycle, NativeClock, WaitTicks};
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::cpu_m68k::M68020;
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::nubus::NubusCard;
use crate::mac::nubus::mdc12::Mdc12;
use crate::mac::nubus::se30video::SE30Video;
use crate::mac::nubus::toby::Toby;
use crate::mac::rtc::Rtc;
use crate::mac::scc::{RTXC_FREQUENCY, Scc};
use crate::mac::scsi::controller::ScsiController;
use crate::mac::sound::asc::Asc;
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::via_common::{LineState, ViaLine};
use crate::mac::{MacModel, MacMonitor, NubusCardConfig, NubusDeviceKind, WARM_START_SIGNATURE};
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

/// Frequency of the bus of the real hardware, without overclocking: C16M,
/// which the CPU runs at
const BUS_FREQUENCY: Ticks = 15_667_200;

/// Frequency of the master clock (C16M) the VIA clock is derived from
const C16M_FREQUENCY: Ticks = 15_667_200;

/// Wait states of a NuBus transaction, approximate: the 10 MHz NuBus and
/// the crossing between its clock and the CPU's add up to about 7 clocks
const NUBUS_WAITS: Ticks = 4;

/// Time the SCC needs between accesses (2.2us)
const SCC_RECOVERY: Ticks = BUS_FREQUENCY * 22 / 10_000_000;

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

struct RamConfig {
    size: usize,
    expected_sz: u8,
    mirror: bool,
}

const RAMSZ_256K: u8 = 0;
const RAMSZ_1M: u8 = 1;
const RAMSZ_4M: u8 = 2;
const RAMSZ_16M: u8 = 3;

/// What answers in a 16MB block of the address space
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    None,
    Ram,
    Rom,
    /// ROM in place of the RAM during the boot overlay
    RomOverlay,
    /// I/O devices, repeating every 128KB
    Io,
    /// Test software region / extension ROM
    ExtRom,
    /// NuBus super slots
    SuperSlot,
    /// NuBus standard slots $9-$E
    Slot,
}

const fn region(overlay: bool, block: usize) -> Region {
    match block {
        0x00..=0x4F if overlay => Region::RomOverlay,
        0x00..=0x3F => Region::Ram,
        0x40..=0x4F => Region::Rom,
        0x50 => Region::Io,
        // In the undecoded $51000000-$5FFFFFFF on the real thing
        0x58..=0x5F => Region::ExtRom,
        0x60..=0xEF => Region::SuperSlot,
        0xF9..=0xFE => Region::Slot,
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
pub struct MacIIBus<TRenderer: Renderer, const AMU: bool> {
    /// The currently emulated Macintosh model
    model: MacModel,

    rom: Vec<u8>,
    extension_rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via1: Via,
    pub(crate) via2: Via2,
    pub(crate) scc: Scc,
    pub(crate) asc: Asc,
    /// Native time the SCC can be accessed again
    scc_ready: Ticks,
    asc_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,

    ram_mask: usize,
    ram_mirror: bool,
    ram_expected_ramsiz: u8,
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

    /// NuBus cards (base address: $9)
    nubus_devices: [Option<NubusCard<TRenderer>>; 6],

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer, const AMU: bool> MacIIBus<TRenderer, AMU>
where
    TRenderer: Renderer,
{
    /// RAM configuration properties
    const RAM_CONFIG: [RamConfig; 8] = [
        RamConfig {
            #[allow(clippy::identity_op)]
            size: 1 * 1024 * 1024,
            expected_sz: RAMSZ_256K,
            mirror: false,
        },
        RamConfig {
            size: 2 * 1024 * 1024,
            expected_sz: RAMSZ_256K,
            mirror: true,
        },
        RamConfig {
            size: 4 * 1024 * 1024,
            expected_sz: RAMSZ_1M,
            mirror: false,
        },
        RamConfig {
            size: 8 * 1024 * 1024,
            expected_sz: RAMSZ_1M,
            mirror: true,
        },
        RamConfig {
            size: 16 * 1024 * 1024,
            expected_sz: RAMSZ_4M,
            mirror: false,
        },
        RamConfig {
            size: 32 * 1024 * 1024,
            expected_sz: RAMSZ_4M,
            mirror: true,
        },
        RamConfig {
            // Doesn't work on MacII? Works on IIsi
            size: 64 * 1024 * 1024,
            expected_sz: RAMSZ_16M,
            mirror: false,
        },
        RamConfig {
            size: 128 * 1024 * 1024,
            expected_sz: RAMSZ_16M,
            mirror: true,
        },
    ];

    /// Value to return on open bus
    /// Certain applications (e.g. Animation Toolkit) rely on this.
    const OPENBUS: u8 = 0;

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
        nubus: &[NubusCardConfig],
        extension_rom: Option<&[u8]>,
        mut renderers: Vec<TRenderer>,
        monitor: MacMonitor,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
    ) -> Self {
        // Build the NuBus cards from the provided configuration.
        let mut nubus_devices: [Option<NubusCard<TRenderer>>; 6] = core::array::from_fn(|_| None);
        for cfg in nubus {
            assert!(
                model.nubus_slots().contains(&cfg.slot),
                "NuBus slot {:#X} not available on {}",
                cfg.slot,
                model
            );
            let idx = (cfg.slot - 0x9) as usize;
            // TODO non-video cards
            let renderer = renderers
                .pop()
                .expect("not enough renderers for NuBus cards");
            log::info!("NuBus slot {:#X}: {}", cfg.slot, cfg.kind);
            nubus_devices[idx] = Some(match cfg.kind {
                NubusDeviceKind::Mdc12 => NubusCard::MDC12(Mdc12::new(cfg.rom, renderer, monitor)),
                NubusDeviceKind::Toby => NubusCard::Toby(Toby::new(cfg.rom, renderer)),
                NubusDeviceKind::SE30Video => {
                    NubusCard::SE30Video(SE30Video::new(cfg.rom, renderer))
                }
            });
        }
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
        let ram_config = Self::RAM_CONFIG
            .iter()
            .find(|c| c.size == ram_size)
            .unwrap_or_else(|| panic!("Unsupported RAM size: {}", ram_size));

        if extension_rom.is_some() {
            log::info!("Extension ROM present");
        }

        let mut bus = Self {
            model,

            rom: Vec::from(rom),
            extension_rom: extension_rom.map(Vec::from).unwrap_or_default(),
            ram: vec![0; ram_size],
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            via1: Via::new(model),
            via2: Via2::new(model),
            scc_ready: 0,
            scc: Scc::new(RTXC_FREQUENCY),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), BUS_FREQUENCY),
            scsi: ScsiController::new(),
            asc: Asc::default(),
            asc_clock: 0,
            mouse_ready: false,

            ram_mask: usize::MAX,
            ram_mirror: ram_config.mirror,
            ram_expected_ramsiz: ram_config.expected_sz,
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

            nubus_devices,
            mouse_mode,
        };

        bus.disable_memtest();

        // Initialize ADB devices
        bus.via1.adb.add_device(AdbMouse::new());
        bus.via1.adb.add_device(AdbKeyboard::new());

        bus
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        // TODO multiple cards
        assert_eq!(self.nubus_devices.iter().flatten().count(), 1);
        if let Some(card) = self.nubus_devices.iter_mut().flatten().next() {
            let slot = card.slot();
            slot.set_renderer(renderer);
            slot.render().unwrap();
        }

        self.asc.after_deserialize();

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / crate::mac::compact::bus::RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.via1.rtc.effective_speed()
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        // Audio sample frequency is tied to monitor's horizontal sync
        // 370 horizontal lines * 60.147 frames/sec = 22.254 KHz
        // Round down to a safe commonly used value (22050), 0.9% off
        let sink = provider
            .create_stream(22050, AUDIO_CHANNELS as u8, AUDIO_BUFFER_SAMPLES as u16)
            .unwrap_or_else(|_| null_audio_sink());
        self.asc.set_sink(sink);

        self.scsi.bus_mut().set_audio_provider(provider)
    }

    /// Makes the ROM skip its memory test
    fn disable_memtest(&mut self) {
        let (addr, value) = WARM_START_SIGNATURE;
        info!("Skipping memory test");
        self.write_ram(addr, value);
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

    /// Wait states of a RAM or ROM access: one for the memory, and one for
    /// the address translation by the AMU or PMMU of the 68020 machines
    fn mem_waits(&self) -> Ticks {
        if self.model.cpu_type() == M68020 {
            2
        } else {
            1
        }
    }

    // RAM and ROM transfers stay within an aligned long word, which is in
    // one RAM page: when it is all there, it is one access

    fn ram_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let idx = addr as usize & self.ram_mask;
        if let Some(bytes) = self.ram.get(idx..idx + len as usize) {
            return Some(bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b)));
        }
        Some((addr..addr + len).fold(0, |v, a| {
            let idx = a as usize & self.ram_mask;
            (v << 8) | u32::from(self.ram.get(idx).copied().unwrap_or(Self::OPENBUS))
        }))
    }

    fn ram_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        let idx = addr as usize & self.ram_mask;
        if let Some(ram) = self.ram.get_mut(idx..idx + len as usize) {
            ram.copy_from_slice(&val.to_be_bytes()[4 - len as usize..]);
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
            return Some(());
        }
        for a in addr..addr + len {
            let idx = a as usize & self.ram_mask;
            // Ignore silently to avoid a lot of spam during memory tests
            if idx < self.ram.len() {
                self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
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
        // Open bus on every byte
        Some(u32::MAX)
    }

    fn superslot_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        log::warn!(
            "Unimplemented write to NuBus super slot: {:08X} {:08X}",
            addr,
            val
        );
        Some(())
    }

    fn slot_card(&mut self, addr: Address) -> Option<&mut NubusCard<TRenderer>> {
        self.nubus_devices[(addr >> 24) as usize - 0xF9].as_mut()
    }

    /// The cards are reached a byte at a time
    fn slot_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let card = self.slot_card(addr)?;
        Some((addr..addr + len).fold(0, |v, a| {
            (v << 8) | u32::from(Self::dev_read(a, card.read(a & 0xFF_FFFF)))
        }))
    }

    fn slot_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        let card = self.slot_card(addr)?;
        for a in addr..addr + len {
            let val = (val >> (8 * (addr + len - 1 - a))) as Byte;
            Self::dev_write(a, val, card.write(a & 0xFF_FFFF, val));
        }
        Some(())
    }

    fn via1_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(Self::dev_read(addr, self.via1.read(addr)).into())
    }

    fn via1_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        Self::dev_write(addr, val as Byte, self.via1.write(addr, val as Byte));
        self.sync_via1_pa();
        if self.model == MacModel::SE30 {
            let Some(NubusCard::SE30Video(d)) = self.nubus_devices[5].as_mut() else {
                unreachable!()
            };
            d.vblank_enable = !self.via1.pb().se30_vblank_enable();
            d.fb_select = self.via1.pa().page2();
        }
        Some(())
    }

    /// Syncs the devices driven by the VIA1 port A pins
    fn sync_via1_pa(&mut self) {
        if self.overlay && !self.via1.pa().overlay() {
            debug!("Overlay off");
            self.overlay = false;
        }
        self.swim.sel = self.via1.pa().sel();
        self.swim.intdrive = self.via1.pa().drivesel();
    }

    fn via2_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(Self::dev_read(addr, self.via2.read(addr)).into())
    }

    fn via2_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        Self::dev_write(addr, val as Byte, self.via2.write(addr, val as Byte));

        // Lazy update ramsize
        if self.via2.pa().v2ram0() == self.ram_expected_ramsiz {
            self.ram_mask = if self.ram_mirror {
                self.ram.len() - 1
            } else {
                usize::MAX
            };
        } else {
            self.ram_mask = match self.model {
                MacModel::MacII => {
                    // Just cut everything short to 1MB so the detection proceeds
                    (/* 1 * */1024 * 1024) - 1
                }
                _ => {
                    // Newer models need all RAM visible and do detection differently
                    // Mirroring breaks RAM detection on IIsi, SE/30, etc.
                    // Probably a difference in the newer FDHD ROM and up
                    usize::MAX
                }
            };
        }
        Some(())
    }

    fn scc_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(Self::dev_read(addr, self.scc.read(addr >> 1)).into())
    }

    fn scc_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        Self::dev_write(addr, val as Byte, self.scc.write(addr >> 1, val as Byte))
    }

    fn scsi_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(Self::dev_read(addr, self.scsi.read(addr)).into())
    }

    fn scsi_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        Self::dev_write(addr, val as Byte, self.scsi.write(addr, val as Byte))
    }

    fn scsi_dma_read(&mut self, _addr: Address, _len: u32) -> Option<u32> {
        Some(self.scsi.read_dma().into())
    }

    fn scsi_dma_write(&mut self, _addr: Address, _len: u32, val: u32) -> Option<()> {
        self.scsi.write_dma(val as Byte);
        Some(())
    }

    fn asc_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(Self::dev_read(addr, self.asc.read(addr & 0xFFF)).into())
    }

    fn asc_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        Self::dev_write(addr, val as Byte, self.asc.write(addr & 0xFFF, val as Byte))
    }

    fn swim_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        Some(Self::dev_read(addr, self.swim.read(addr)).into())
    }

    fn swim_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        Self::dev_write(addr, val as Byte, self.swim.write(addr, val as Byte))
    }

    /// I/O devices are byte-wide, 8-bit ports. Their cycles take native
    /// time: the VIAs are synchronous with the VIA clock, the other devices
    /// start on a native tick, and the GLUE inserts one wait state for the
    /// IWM and the SCSI controller (without handshake), two for ASC reads and
    /// one for ASC writes.
    fn io_cycle(&mut self, acc: BusAccess<Bus68020>) -> BusResponse {
        let device = (acc.addr & 0x1_FFFF) >> 13;
        match device {
            0 => return self.via_cycle(acc, Self::via1_read, Self::via1_write),
            1 => return self.via_cycle(acc, Self::via2_read, Self::via2_write),
            _ => (),
        }
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        match device {
            2 => self.scc_cycle(acc, w),
            // SCSI pseudo-DMA only takes the lower 4KB of its slot
            3 if acc.addr & 0x1000 == 0 => {
                bus_access_byte(acc, w, self, Self::scsi_dma_read, Self::scsi_dma_write)
            }
            8 => {
                let w = self.native_clock.scale_dev_waits(w, 1);
                bus_access_byte(acc, w, self, Self::scsi_read, Self::scsi_write)
            }
            9 => bus_access_byte(acc, w, self, Self::scsi_dma_read, Self::scsi_dma_write),
            10 => {
                let w = self
                    .native_clock
                    .scale_dev_waits(w, if acc.write { 1 } else { 2 });
                bus_access_byte(acc, w, self, Self::asc_read, Self::asc_write)
            }
            11 => {
                let hold = self.swim.hold(&self.native_clock);
                let w = self.native_clock.scale_dev_waits(w, 1 + hold);
                bus_access_byte(acc, w, self, Self::swim_read, Self::swim_write)
            }
            // Expansion area
            _ => BusResponse::BusError,
        }
    }

    /// The GLUE holds off an access to the SCC until it has had the time it
    /// needs since the previous one
    fn scc_cycle(&mut self, acc: BusAccess<Bus68020>, w: WaitTicks) -> BusResponse {
        // When overclocking, acc.pending is 0 after device_sync(), and native
        // ticks are CPU ticks otherwise
        let now = self.native_clock.native_ticks() + acc.pending;
        let hold = self.scc_ready.saturating_sub(now);
        // The access itself takes 3 clocks
        self.scc_ready = now + hold + 3 + SCC_RECOVERY;
        let w = self.native_clock.scale_dev_waits(w, hold);
        bus_access_byte(acc, w, self, Self::scc_read, Self::scc_write)
    }

    /// VIA accesses are synchronous with the VIA clock: one starts on a VIA
    /// tick and lasts a VIA clock cycle
    fn via_cycle(
        &mut self,
        acc: BusAccess<Bus68020>,
        read: fn(&mut Self, Address, u32) -> Option<u32>,
        write: fn(&mut Self, Address, u32, u32) -> Option<()>,
    ) -> BusResponse {
        match self.native_clock.via_sync(&acc, C16M_PER_VIA / 2) {
            DeviceCycle::Stall(n) => BusResponse::Stall(n),
            DeviceCycle::Access(w) => bus_access_byte(acc, w, self, read, write),
        }
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

        // Report updated mouse coordinates to OS
        self.write_ram(Self::ADDR_MTEMP_X, x);
        self.write_ram(Self::ADDR_MTEMP_Y, y);
        if self.model >= MacModel::SE {
            // SE+ needs to see even a small difference between the current (RawMouse)
            // and new (MTemp) position, otherwise the change is ignored.
            self.write_ram(Self::ADDR_RAWMOUSE_X, x.wrapping_add_signed(-1));
            self.write_ram(Self::ADDR_RAWMOUSE_Y, y.wrapping_add_signed(1));
        } else {
            self.write_ram(Self::ADDR_RAWMOUSE_X, x);
            self.write_ram(Self::ADDR_RAWMOUSE_Y, y);
        }
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

    fn amu_translate(&self, addr: Address) -> Address {
        if !AMU {
            return addr;
        }

        match addr & 0xFFFFFF {
            0x00_0000..=0x7F_FFFF => addr & 0x7F_FFFF,
            0x80_0000..=0x8F_FFFF => 0x4000_0000 | (addr & 0xF_FFFF),
            0x90_0000..=0x9F_FFFF => 0xF900_0000 | (addr & 0xF_FFFF),
            0xA0_0000..=0xAF_FFFF => 0xFA00_0000 | (addr & 0xF_FFFF),
            0xB0_0000..=0xBF_FFFF => 0xFB00_0000 | (addr & 0xF_FFFF),
            0xC0_0000..=0xCF_FFFF => 0xFC00_0000 | (addr & 0xF_FFFF),
            0xD0_0000..=0xDF_FFFF => 0xFD00_0000 | (addr & 0xF_FFFF),
            0xE0_0000..=0xEF_FFFF => 0xFE00_0000 | (addr & 0xF_FFFF),
            0xF0_0000..=0xF7_FFFF => 0x5000_0000 | (addr & 0xF_FFFF),
            0xF8_0000..=0xF9_FFFF => 0x5800_0000 | (addr & 0x1_FFFF),
            0xFA_0000..=0xFF_FFFF => 0x5000_0000 | (addr & 0xF_FFFF),
            _ => unreachable!(),
        }
    }

    pub fn video_blank(&mut self) -> Result<()> {
        for d in self.nubus_devices.iter_mut().flatten() {
            d.slot().blank()?;
        }
        Ok(())
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

impl<TRenderer, const AMU: bool> Bus<Address, Byte> for MacIIBus<TRenderer, AMU>
where
    TRenderer: Renderer,
{
    type Cpu = Bus68020;

    fn get_mask(&self) -> Address {
        0xFFFFFFFF
    }

    #[inline(always)]
    fn access(&mut self, mut acc: BusAccess<Bus68020>) -> Result<BusResponse> {
        let amu = AMU && self.amu_active;
        let vaddr = acc.addr;
        if amu {
            acc.addr = self.amu_translate(acc.addr);
        }

        let overlay = self.overlay && !amu;
        let mem_waits = self.mem_waits();
        let resp = match MAP[usize::from(overlay)][(acc.addr >> 24) as usize] {
            Region::Ram => bus_access_long(acc, mem_waits, self, Self::ram_read, Self::ram_write),
            Region::Rom => {
                bus_access_long(acc, mem_waits, self, Self::rom_read, Self::ignore_write)
            }
            Region::RomOverlay => {
                bus_access_long(acc, mem_waits, self, Self::rom_read, Self::no_write)
            }
            Region::Io => self.io_cycle(acc),
            Region::ExtRom => {
                bus_access_long(acc, mem_waits, self, Self::ext_rom_read, Self::no_write)
            }
            Region::SuperSlot => bus_access_long(
                acc,
                NUBUS_WAITS,
                self,
                Self::superslot_read,
                Self::superslot_write,
            ),
            Region::Slot => {
                bus_access_long(acc, NUBUS_WAITS, self, Self::slot_read, Self::slot_write)
            }
            Region::None => BusResponse::BusError,
        };

        if resp == BusResponse::BusError {
            let rw = if acc.write { "write" } else { "read" };
            if amu {
                warn!(
                    "Bus error ({}) at unmapped address: {:06X} -> {:08X}",
                    rw,
                    vaddr & 0xFFFFFF,
                    acc.addr
                );
            } else {
                warn!("Bus error ({}) at unmapped address: {:08X}", rw, acc.addr);
            }
        }
        Ok(resp)
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            // Clear RAM
            self.ram.fill(0);

            self.disable_memtest();

            self.ram_dirty
                .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
        }

        // Keep the RTC and ADB for PRAM and event channels
        let Via { adb, rtc, .. } = std::mem::replace(&mut self.via1, Via::new(self.model));
        self.via1.adb = adb;
        self.via1.rtc = rtc;

        self.scc = Scc::new(RTXC_FREQUENCY);
        self.via2 = Via2::new(self.model);
        for d in self.nubus_devices.iter_mut().filter_map(|f| f.as_mut()) {
            d.slot().reset();
        }
        self.asc.reset();

        self.amu_active = false;
        self.mouse_ready = false;
        self.overlay = true;
        self.sync_via1_pa();
        Ok(false)
    }
}

impl<TRenderer, const AMU: bool> Tickable for MacIIBus<TRenderer, AMU>
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

        if AMU {
            self.amu_active = !self.via2.pb().vfc3();
        }

        // The devices run at the native frequency, whatever the CPU is
        // overclocked to.
        //
        // The Mac II generates the VIA clock through some dividers on the logic board.
        // This same logic generates wait states when the VIAs are accessed.
        //
        // The VIA clock is C16M / 20 (783.36 kHz), once every 20 native ticks.
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

                // Legacy VBlank interrupt (60.15 Hz)
                //
                // This is the square wave output of VIA2 timer 1 on PB7, which is
                // wired to CA1 of VIA1.
                self.via1
                    .set_line(ViaLine::CA1, self.via2.pb().v2vbl().into());
            }
        }

        // Host frame pacing
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

        // Audio
        // The interrupt output of the ASC is active low
        self.via2
            .set_line(ViaLine::CB1, (!self.asc.get_irq()).into());

        self.asc_clock += native_ticks * C16M_FREQUENCY;
        while self.asc_clock >= self.asc.sample_period() * ctx.bus_frequency {
            self.asc_clock -= self.asc.sample_period() * ctx.bus_frequency;

            self.asc.tick(self.speed == EmulatorSpeed::Accurate)?;
        }

        // NuBus slot IRQs and ticks
        let mut slot_irqs = 0;
        for slot in 0..(self.nubus_devices.len()) {
            if let Some(dev) = self.nubus_devices[slot].as_mut() {
                dev.tick(native_ticks, ctx)?;
                if dev.get_irq() {
                    slot_irqs |= 1 << slot;
                }
            }
        }
        self.via2.a_in.set_v2irqs(!slot_irqs);
        // The GLUE combines the slot interrupts into /SLOTIRQ
        self.via2.set_line(
            ViaLine::CA1,
            if slot_irqs > 0 {
                LineState::Low
            } else {
                LineState::High
            },
        );

        self.scsi.tick(native_ticks, ctx)?;

        // IRQ and DRQ of the SCSI controller are active high and directly
        // connected to CB2 and CA2
        self.via2.set_line(ViaLine::CB2, self.scsi.get_irq().into());
        self.via2.set_line(ViaLine::CA2, self.scsi.get_drq().into());

        self.swim.tick(native_ticks, ())?;
        self.scc.tick(native_ticks, ctx.bus_frequency);

        Ok(1)
    }
}

impl<TRenderer, const AMU: bool> IrqSource for MacIIBus<TRenderer, AMU>
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

impl<TRenderer, const AMU: bool> InspectableBus<Address, Byte> for MacIIBus<TRenderer, AMU>
where
    TRenderer: Renderer,
{
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        // Everything up to 0x4FFFFFFF is safe (RAM/ROM only)
        let val = match MAP[usize::from(self.overlay)][(addr >> 24) as usize] {
            Region::Ram => self.ram_read(addr, 1),
            Region::Rom | Region::RomOverlay => self.rom_read(addr, 1),
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

impl<TRenderer, const AMU: bool> Debuggable for MacIIBus<TRenderer, AMU>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_group, dbgprop_nest};

        let mut result = vec![
            dbgprop_nest!("Apple Desktop Bus", self.via1.adb),
            dbgprop_nest!("Apple Sound Chip", self.asc),
            dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA 1 (SY6522)", self.via1),
            dbgprop_nest!("VIA 2 (SY6522)", self.via2),
        ];

        for (i, slot) in self.nubus_devices.iter().enumerate() {
            if let Some(dev) = slot.as_ref() {
                result.push(dbgprop_nest!(
                    format!("NuBus slot ${:1X} ({})", i + 0x09, dev.to_string()),
                    dev
                ));
            } else {
                result.push(dbgprop_group!(
                    format!("NuBus slot ${:1X} (empty)", i + 0x09),
                    vec![]
                ));
            }
        }
        result
    }
}
