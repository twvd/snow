use std::thread;
use std::time::{Duration, Instant};

use crate::bus::access::{Bus68040, BusAccess, BusResponse, bus_access_long};
use crate::bus::native_clock::NativeClock;
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::macii::via2::Via2;
use crate::mac::quadra::dafb::Dafb;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::esp::Esp;
use crate::mac::sound::easc::Easc;
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::via_common::{LineState, ViaLine};
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
/// 25 MHz of the 68040
const BUS_FREQUENCY: Ticks = 25_000_000;

/// SCC serial controller clock (PCLK)
pub const SCC_CLOCK: Ticks = 3_672_000;

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
    /// Test software region / extension ROM
    ExtRom,
    /// NuBus super slots
    SuperSlot,
    /// Built-in video (DAFB), slot $9
    Video,
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
pub struct Quadra700Bus<TRenderer: Renderer> {
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
    pub(crate) asc: Easc,
    asc_clock: Ticks,
    scc_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: Esp,

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
    pub(crate) dafb: Dafb<TRenderer>,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer> Quadra700Bus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Value to return on open bus
    /// Certain applications (e.g. Animation Toolkit) rely on this.
    const OPENBUS: u8 = 0;

    const IO_MASK: Address = 0x3_FFFF;

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
        extension_rom: Option<&[u8]>,
        renderer: TRenderer,
        monitor: MacMonitor,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());

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
            scc_clock: 0,
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), BUS_FREQUENCY),
            // MODE pins strapped for a 16-bit DMA bus (schematic 050-8020)
            scsi: Esp::new(true),
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

            dafb: Dafb::new(renderer, monitor),
            mouse_mode,
        };

        // Disable memory test
        if let Some((addr, value)) = model.disable_memtest() {
            info!("Skipping memory test");
            bus.write_ram(addr, value);
        }

        // Initialize ADB devices
        bus.via1.adb.add_device(AdbMouse::new());
        bus.via1.adb.add_device(AdbKeyboard::new());

        bus
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        self.dafb.renderer = Some(renderer);
        self.dafb.render().unwrap();

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
        let sink = provider
            .create_stream(
                self.asc.sample_rate() as i32,
                AUDIO_CHANNELS as u8,
                AUDIO_BUFFER_SAMPLES as u16,
            )
            .unwrap_or_else(|_| null_audio_sink());
        self.asc.set_sink(sink);

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
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
            return Some(());
        }
        for a in addr..addr + len {
            let idx = a as usize;
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
            // Ethernet controller
            0x0000_8000..=0x0000_8007 | 0x0000_A000..=0x0000_B0FF => Some(0),
            // SCC
            0x0000_C000..=0x0000_DFFF => Some(Self::dev_read(addr, self.scc.read(addr >> 1))),
            // Orwell memory controller
            0x0000_E000..=0x0000_EFFF => Some(0),
            // SCSI (NCR 53C96)
            0x0000_F000..=0x0000_F0FF => Some(Self::dev_read(addr, self.scsi.read(addr))),
            0x0000_F100..=0x0000_F1FF => Some(self.scsi.read_dma()),
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
            // Ethernet controller
            0x0000_8000..=0x0000_8007 | 0x0000_A000..=0x0000_B0FF => Some(()),
            // SCC
            0x0000_C000..=0x0000_DFFF => Self::dev_write(addr, val, self.scc.write(addr >> 1, val)),
            // Orwell memory controller
            0x0000_E000..=0x0000_EFFF => Some(()),
            // SCSI (NCR 53C96)
            0x0000_F000..=0x0000_F0FF => Self::dev_write(addr, val, self.scsi.write(addr, val)),
            0x0000_F100..=0x0000_F1FF => Some(self.scsi.write_dma(val)),
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

impl<TRenderer> Bus<Address, Byte> for Quadra700Bus<TRenderer>
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

        let resp = match MAP[usize::from(self.overlay)][block] {
            Region::Ram => bus_access_long(acc, 0, self, Self::ram_read, Self::ram_write),
            Region::Rom => bus_access_long(acc, 0, self, Self::rom_read, Self::ignore_write),
            Region::Io => bus_access_long(acc, 0, self, Self::io_read, Self::io_write),
            Region::ExtRom => bus_access_long(acc, 0, self, Self::ext_rom_read, Self::no_write),
            Region::SuperSlot => {
                bus_access_long(acc, 0, self, Self::superslot_read, Self::superslot_write)
            }
            Region::Video => bus_access_long(acc, 0, self, Self::video_read, Self::video_write),
            Region::None => BusResponse::BusError,
        };

        if resp == BusResponse::BusError {
            let rw = if acc.write { "write" } else { "read" };
            warn!("Bus error ({}) at unmapped address: {:08X}", rw, acc.addr);
        }
        Ok(resp)
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            // Clear RAM
            self.ram.fill(0);

            // Disable memory test
            if let Some((addr, value)) = self.model.disable_memtest() {
                self.write_ram(addr, value);
            }

            self.ram_dirty
                .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
        }

        // Keep the RTC and ADB for PRAM and event channels
        let Via { adb, rtc, .. } = std::mem::replace(&mut self.via1, Via::new(self.model));
        self.via1.adb = adb;
        self.via1.rtc = rtc;

        self.scc = Scc::new();
        self.via2 = Via2::new(self.model);
        self.dafb.reset();
        self.asc.reset();

        self.amu_active = false;
        self.mouse_ready = false;
        self.overlay = true;
        self.sync_via1_pa();
        Ok(false)
    }
}

impl<TRenderer> Tickable for Quadra700Bus<TRenderer>
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
            }
        }

        // Legacy VBlank interrupt, a 60 Hz square wave on CA1 of VIA 1
        self.vblank_clock += native_ticks;
        if self.vblank_clock >= ctx.bus_frequency / 120 {
            self.via1.set_line(ViaLine::CA1, LineState::High);
        }
        while self.vblank_clock >= ctx.bus_frequency / 60 {
            self.vblank_clock -= ctx.bus_frequency / 60;

            self.via1.set_line(ViaLine::CA1, LineState::Low);

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

        // SCC baud rate generator
        self.scc_clock += native_ticks * SCC_CLOCK;
        let scc_ticks = self.scc_clock / ctx.bus_frequency;
        if scc_ticks > 0 {
            self.scc_clock -= scc_ticks * ctx.bus_frequency;
            self.scc.tick_brg(scc_ticks);
        }

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
        self.via2.set_line(
            ViaLine::CA1,
            if video_irq {
                LineState::Low
            } else {
                LineState::High
            },
        );

        self.scsi.tick(native_ticks, ctx)?;

        // The interrupt output of the 53C96 is active low
        self.via2
            .set_line(ViaLine::CB2, (!self.scsi.get_irq()).into());
        self.via2.set_line(ViaLine::CA2, self.scsi.get_drq().into());

        self.swim.intdrive = self.via1.pa().drivesel();
        self.swim.tick(native_ticks, ())?;

        Ok(1)
    }
}

impl<TRenderer> IrqSource for Quadra700Bus<TRenderer>
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

impl<TRenderer> InspectableBus<Address, Byte> for Quadra700Bus<TRenderer>
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

impl<TRenderer> Debuggable for Quadra700Bus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_group, dbgprop_nest};

        let mut result = vec![
            dbgprop_nest!("Apple Desktop Bus", self.via1.adb),
            dbgprop_nest!("Enhanced Apple Sound Chip", self.asc),
            dbgprop_nest!("Built-in video (DAFB)", self.dafb),
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
