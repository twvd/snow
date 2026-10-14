use std::thread;
use std::time::{Duration, Instant};

use super::normandy::Normandy;
use super::pmgr::Pmgr;
use super::via::Via;
use super::video::Video;
use crate::bus::access::{Bus68000, BusAccess, BusResponse, bus_access_byte_lane, bus_access_word};
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::asc::Asc;
use crate::mac::macii::bus::DEFAULT_BUS_SPEED;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::via_common::{LineState, ViaLine};
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

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

/// Size of the blocks of the address map
const MAP_BLOCK_SHIFT: u32 = 16;
const MAP_BLOCKS: usize = 1 << (24 - MAP_BLOCK_SHIFT);

/// What answers in a block of the address map
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    None,
    Ram,
    /// Writes ignored
    Rom,
    /// ROM on reads, RAM on writes
    RomOverRam,
    /// Reads unimplemented, writes ignored
    Ignored,
    /// SLIM ROM, writes ignored
    SlimRom,
    Normandy,
    Swim,
    Via,
    /// Test software region / extension ROM
    ExtRom,
    Scsi,
    Video,
    Asc,
    Scc,
}

/// Address map: the region of each block, with the boot overlay off and on
type AddressMap = [[Region; MAP_BLOCKS]; 2];

/// Wait states of a bus cycle in progress
#[derive(Default)]
struct Stalled {
    /// Wait states so far
    waits: Ticks,
    /// Bytes past their wait states
    bytes: u32,
    /// The second byte has been stalled for, 2 cycles into the bus cycle
    second: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct MacPortableBus<TRenderer: Renderer> {
    cycles: Ticks,

    /// The currently emulated Macintosh model
    model: MacModel,

    rom_mask: usize,
    rom: Vec<u8>,
    extension_rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via: Via,
    via_clock: Ticks,
    pub(crate) scc: Scc,
    pub(crate) video: Video<TRenderer>,
    pub(crate) asc: Asc,
    asc_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    pub(crate) pmgr: Pmgr,
    normandy: Normandy,

    overlay: bool,

    #[serde(skip, default = "empty_map")]
    map: AddressMap,

    /// Not saved, as a cycle is never in progress between instructions
    #[serde(skip)]
    stalled: Stalled,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    bus_frequency: Ticks,

    /// Last vblank time (for syncing to video)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for in Video speed mode.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,
    vblank_clock: Ticks,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer> MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
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

    pub fn new(
        model: MacModel,
        rom: &[u8],
        extension_rom: Option<&[u8]>,
        renderer: TRenderer,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());

        if extension_rom.is_some() {
            info!("Extension ROM present");
        }

        let rom = {
            let mut rom = rom.to_vec();

            // Patch the ROM if using the 15MB RAM mod
            if model == MacModel::Portable15MB {
                rom[0x01..=0x03].copy_from_slice(&[0xC9, 0xE4, 0xB1]);
                rom[0x05..=0x07].copy_from_slice(&[0xF3, 0xFF, 0x70]);
                rom[0x67] = 0xF0;
                rom[0x3EBD] = 0xA0;
                rom[0x29D33] = 0xF8;
                rom[0x29DB3] = 0xF8;
                rom[0x3FF70..=0x3FF7B].copy_from_slice(&[
                    0x4A, 0x39, 0x00, 0x90, 0x00, 0x2A, 0x4E, 0xF9, 0x00, 0xF0, 0x00, 0x2A,
                ]);
            }

            rom
        };

        let mut bus = Self {
            cycles: 0,
            model,

            rom_mask: rom.len() - 1,
            rom,
            extension_rom: extension_rom.map(Vec::from).unwrap_or_default(),
            ram: vec![0; ram_size],
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            via: Via::new(),
            via_clock: 0,
            video: Video::new(renderer),
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), 16_000_000),
            scsi: ScsiController::new(),
            asc: Asc::default(),
            asc_clock: 0,
            mouse_ready: false,

            overlay: true,
            map: build_map(model),
            stalled: Stalled::default(),
            speed: EmulatorSpeed::Accurate,
            bus_frequency: DEFAULT_BUS_SPEED,
            vblank_time: Instant::now(),
            vblank_clock: 0,
            progkey_pressed: LatchingEvent::default(),
            mouse_mode,
            pmgr: Pmgr::new(),
            normandy: Normandy::new(),
        };

        // Disable memory test
        if let Some((addr, value)) = model.disable_memtest() {
            info!("Skipping memory test");
            bus.write_ram(addr, value);
        }

        bus.pmgr.adb_add_device(AdbMouse::new());
        bus.pmgr.adb_add_device(AdbKeyboard::new());

        bus
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, _renderer: TRenderer) {
        self.map = build_map(self.model);
        self.asc.after_deserialize();

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / crate::mac::compact::bus::RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.pmgr.rtc.effective_speed()
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        let sink = provider
            .create_stream(22050, AUDIO_CHANNELS as u8, AUDIO_BUFFER_SAMPLES as u16)
            .unwrap_or_else(|_| null_audio_sink());
        self.asc.set_sink(sink);

        self.scsi.set_audio_provider(provider)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn write_ram<T: ToBytes>(&mut self, addr: Address, val: T) {
        let addr = addr as usize;
        let bytes = val.to_be_bytes();
        for (i, &b) in bytes.as_ref().iter().enumerate() {
            self.ram[addr + i] = b;
            self.ram_dirty
                .insert((addr + i) / crate::mac::macii::bus::RAM_DIRTY_PAGESIZE);
        }
    }

    fn read_ram<T: PrimInt + FromPrimitive>(&self, addr: Address) -> T {
        let addr = addr as usize;
        let len = size_of::<T>();
        let end = addr + len;

        assert!(len <= 4);
        let mut tmp = [0_u8; 4];
        tmp[4 - len..].copy_from_slice(&self.ram[addr..end]);
        T::from_u32(u32::from_be_bytes(tmp)).unwrap()
    }

    /// Ends the boot overlay on an access to the normal location of the ROM
    fn end_overlay(&mut self, addr: Address) {
        if (0x0090_0000..=0x009F_FFFF).contains(&addr) {
            self.overlay = false;
        }
    }

    fn region(&self, addr: Address) -> Region {
        self.map[usize::from(self.overlay)][(addr >> MAP_BLOCK_SHIFT) as usize]
    }

    fn read_byte(&mut self, region: Region, addr: Address) -> Option<Byte> {
        match region {
            Region::Ram => self.normal_ram_read(addr),
            Region::Rom | Region::RomOverRam => {
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            Region::SlimRom | Region::Normandy => self.normandy.read(addr),
            Region::ExtRom => Some(
                *self
                    .extension_rom
                    .get((addr - 0xF8_0000) as usize)
                    .unwrap_or(&0xFF),
            ),
            Region::Video => Some(self.video.framebuffer[(addr & 0x7FFF) as usize]),
            Region::Asc => self.asc.read(addr & 0xFFF),
            _ => None,
        }
    }

    fn write_byte(&mut self, region: Region, addr: Address, val: Byte) -> Option<()> {
        match region {
            Region::Ram | Region::RomOverRam => self.normal_ram_write(addr, val),
            Region::Rom | Region::Ignored | Region::SlimRom => Some(()),
            Region::Normandy => self.normandy.write(addr, val),
            Region::Video => Some(self.video.framebuffer[(addr & 0x7FFF) as usize] = val),
            Region::Asc => self.asc.write(addr & 0xFFF, val),
            _ => None,
        }
    }

    fn swim_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        self.swim.read(addr).map(u32::from)
    }

    fn swim_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.swim.write(addr, val as Byte)
    }

    fn via_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        self.via.read(addr).map(u32::from)
    }

    fn via_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.via.write(addr, val as Byte)
    }

    fn scsi_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        self.scsi.read(addr).map(u32::from)
    }

    fn scsi_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.scsi.write(addr, val as Byte)
    }

    fn scc_read(&mut self, addr: Address, _len: u32) -> Option<u32> {
        self.scc.read(addr >> 1).map(u32::from)
    }

    fn scc_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.scc.write(addr >> 1, val as Byte)
    }

    /// Reads the bytes of a cycle one at a time, as the byte interface did
    fn read_bytes(&mut self, region: Region, addr: Address, len: u32) -> u32 {
        (addr..addr + len).fold(0, |v, a| {
            let val = self.read_byte(region, a).unwrap_or_else(|| {
                warn!("Read from unimplemented address: {:08X}", a);
                Self::OPENBUS
            });
            (v << 8) | u32::from(val)
        })
    }

    /// Writes the bytes of a cycle one at a time, as the byte interface did
    fn write_bytes(&mut self, region: Region, addr: Address, len: u32, val: u32) {
        for a in addr..addr + len {
            let val = (val >> (8 * (addr + len - 1 - a))) as Byte;
            if self.write_byte(region, a, val).is_none() {
                warn!("Write to unimplemented address: {:08X} {:02X}", a, val);
            }
        }
    }

    fn normal_ram_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        if addr < self.ram.len() as u32 {
            let idx = addr as usize;
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
            Some(self.ram[idx] = val)
        } else {
            Some(())
        }
    }

    fn normal_ram_read(&self, addr: Address) -> Option<Byte> {
        if addr < self.ram.len() as u32 {
            Some(self.ram[addr as usize])
        } else {
            Some(0x00)
        }
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        if self.mouse_mode == MouseMode::Disabled {
            return;
        }

        if button.is_some() {
            self.pmgr.adb_event(&AdbEvent::Mouse(MouseEvent {
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
                self.pmgr.adb_event(&AdbEvent::Mouse(MouseEvent {
                    button: None,
                    rel_movement: Some((relx.into(), rely.into())),
                }));
            }
            MouseMode::Disabled => unreachable!(),
        }
    }

    /// Updates the mouse position (absolute coordinates)
    pub fn mouse_update_abs(&mut self, x: u16, y: u16) {
        if self.mouse_mode == MouseMode::Disabled {
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

        // Trigger ADB update to disable idle
        if self.normandy.idle_speed {
            self.pmgr.adb_event(&AdbEvent::Mouse(MouseEvent {
                button: None,
                rel_movement: None,
            }));
        }

        // Report updated mouse coordinates to OS
        self.write_ram(Self::ADDR_MTEMP_X, x);
        self.write_ram(Self::ADDR_MTEMP_Y, y);
        // SE+ needs to see even a small difference between the current (RawMouse)
        // and new (MTemp) position, otherwise the change is ignored.
        self.write_ram(Self::ADDR_RAWMOUSE_X, x.wrapping_add_signed(-1));
        self.write_ram(Self::ADDR_RAWMOUSE_Y, y.wrapping_add_signed(1));

        self.write_ram(Self::ADDR_CRSRNEW, 1_u8);
    }

    /// Configures emulator speed
    pub fn set_speed(&mut self, speed: EmulatorSpeed) {
        info!("Emulation speed: {:?}", speed);
        self.speed = speed;
    }

    pub fn set_bus_frequency(&mut self, bus_frequency: u64) {
        self.bus_frequency = bus_frequency;
    }

    pub fn set_mouse_mode(&mut self, mode: MouseMode) {
        self.mouse_mode = mode;
    }

    /// Stall of a bus cycle which has to wait, polled every 2 cycles for
    /// each byte with the bus ticked in between, as the byte interface did,
    /// or None once it can go on
    fn stall(&mut self, acc: &BusAccess<Bus68000>) -> Option<Ticks> {
        let st = &mut self.stalled;
        while st.bytes < acc.size {
            if self.normandy.waitstate(acc.addr + st.bytes) {
                st.waits += 2;
                // The second byte comes 2 cycles into the bus cycle
                if st.bytes > 0 && !st.second {
                    st.second = true;
                    return Some(4);
                }
                return Some(2);
            }
            st.bytes += 1;
        }
        None
    }

    /// Programmer's key pressed
    pub fn progkey(&mut self) {
        self.progkey_pressed.set();
    }

    pub fn video_blank(&mut self) -> Result<()> {
        self.video.blank()
    }

    pub fn keyboard_event(&mut self, ke: KeyEvent) {
        self.pmgr.adb_event(&AdbEvent::Key(ke));
    }

    pub fn input_release_all(&mut self) {
        self.pmgr.adb_event(&AdbEvent::ReleaseAll);
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.pmgr.rtc
    }
}

impl<TRenderer> Bus<Address, Byte> for MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
    type Cpu = Bus68000;
    const SIZED: bool = true;

    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    #[inline(always)]
    fn access(&mut self, acc: BusAccess<Bus68000>) -> Result<BusResponse> {
        if let Some(stall) = self.stall(&acc) {
            return Ok(BusResponse::Stall(stall));
        }
        let waits = std::mem::take(&mut self.stalled).waits;

        self.end_overlay(acc.addr);
        let region = self.region(acc.addr);
        let resp = match region {
            // On the lower byte lane, at odd addresses
            Region::Swim => bus_access_byte_lane::<2, 1, _, _>(
                acc,
                waits,
                self,
                Self::swim_read,
                Self::swim_write,
            ),
            // On the upper byte lane, at even addresses
            Region::Via => bus_access_byte_lane::<2, 0, _, _>(
                acc,
                waits,
                self,
                Self::via_read,
                Self::via_write,
            ),
            // On the upper byte lane, its /IOR and /IOW decoded by the GLU
            Region::Scsi => bus_access_byte_lane::<2, 0, _, _>(
                acc,
                waits,
                self,
                Self::scsi_read,
                Self::scsi_write,
            ),
            // On the upper byte lane, A/B on A1 and D/C on A2
            Region::Scc => bus_access_byte_lane::<2, 0, _, _>(
                acc,
                waits,
                self,
                Self::scc_read,
                Self::scc_write,
            ),
            _ => bus_access_word(
                acc,
                waits,
                self,
                |bus, addr, len| Some(bus.read_bytes(region, addr, len)),
                |bus, addr, len, val| {
                    bus.write_bytes(region, addr, len, val);
                    Some(())
                },
            ),
        };
        let data = match resp {
            BusResponse::Ack { data, .. } => data,
            BusResponse::Stall(_) => unreachable!(),
            BusResponse::BusError if acc.write => {
                warn!(
                    "Write to unimplemented address: {:08X} {:02X}",
                    acc.addr, acc.data
                );
                0
            }
            BusResponse::BusError => {
                warn!("Read from unimplemented address: {:08X}", acc.addr);
                Self::OPENBUS.into()
            }
        };

        if acc.write {
            // Sync values that live in multiple places
            self.swim.sel = self.via.pb().headsel();
        }

        Ok(BusResponse::Ack {
            width: 2,
            data,
            waits,
        })
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            // Clear RAM
            self.ram.fill(0);

            // Disable memory test
            if let Some((addr, value)) = self.model.disable_memtest() {
                self.write_ram(addr, value);
            }
        }

        self.pmgr.reset();

        self.scc = Scc::new();
        self.via.reset();
        self.asc.reset();
        self.mouse_ready = false;
        self.overlay = true;
        Ok(false)
    }

    fn cpu_tick(&mut self, ticks: Ticks) -> Result<()> {
        // This is called from the CPU per bus access (fine-grained).
        // The power manager needs accuracy beyond instruction granularity.
        self.pmgr.a_out = self.via.pa().0;
        self.pmgr.a_in = self.via.a_in.0;
        self.pmgr.pmreq = self.via.pb().pmreq();
        self.pmgr.tick(ticks, ())?;
        self.via.b_in.set_pmack(self.pmgr.pmack);
        self.via.a_in.0 = self.pmgr.a_in;
        if self.pmgr.take_pmint() {
            self.via.set_line(ViaLine::CB1, LineState::Pulse);
        }
        if self.pmgr.take_onesec() {
            self.via.set_line(ViaLine::CA2, LineState::Pulse);
        }

        Ok(())
    }
}

impl<TRenderer> Tickable for MacPortableBus<TRenderer>
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
            bus_frequency: self.bus_frequency,
        };

        self.cycles += ticks;

        self.via_clock += ticks;
        while self.via_clock >= 20 {
            // TODO VIA wait states
            self.via_clock -= 20;

            self.via.tick(1, ctx)?;
        }

        self.video.tick(ticks, ctx)?;

        // Legacy VBlank interrupt
        self.vblank_clock += ticks;
        while self.vblank_clock >= self.bus_frequency / 60 {
            self.vblank_clock -= self.bus_frequency / 60;
            self.via.set_line(ViaLine::CA1, LineState::Pulse);
            self.pmgr.tick_60hz();

            if self.speed == EmulatorSpeed::Video {
                // Sync to 60 fps video
                let frametime = self.vblank_time.elapsed().as_micros() as u64;
                const DESIRED_FRAMETIME: u64 = 1_000_000 / 60;

                self.vblank_time = Instant::now();

                if frametime < DESIRED_FRAMETIME {
                    thread::sleep(Duration::from_micros(DESIRED_FRAMETIME - frametime));
                }
            }
        }

        self.asc_clock += ticks;
        while self.asc_clock >= self.bus_frequency / self.asc.sample_rate() {
            self.asc_clock -= self.bus_frequency / self.asc.sample_rate();

            self.asc.tick(self.speed == EmulatorSpeed::Accurate)?;
        }

        self.swim.intdrive = self.via.pb().drivesel();
        self.swim.tick(ticks, ())?;

        Ok(ticks)
    }
}

impl<TRenderer> IrqSource for MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
        if self.progkey_pressed.get_clear() {
            return Some(4);
        }
        if self.scc.get_irq() | self.asc.get_irq() {
            return Some(2);
        }
        if self.via.irq() {
            return Some(1);
        }
        None
    }
}

impl<TRenderer> InspectableBus<Address, Byte> for MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        if addr >= 0x00F0_0000 {
            return None;
        }
        self.end_overlay(addr);
        self.read_byte(self.region(addr), addr)
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        if addr >= 0x00F0_0000 {
            return None;
        }
        self.end_overlay(addr);
        self.write_byte(self.region(addr), addr, val)
    }
}

fn region(model: MacModel, overlay: bool, addr: Address) -> Region {
    // The 15MB RAM mod remaps RAM and the ROM
    let ram15 = model == MacModel::Portable15MB;
    match addr {
        // ROM in place of the RAM during the boot overlay
        0x0000_0000..=0x000F_FFFF if overlay => Region::RomOverRam,
        0x0000_0000..=0x008F_FFFF => Region::Ram,
        0x0090_0000..=0x00EF_FFFF if ram15 => Region::Ram,
        0x0090_0000..=0x009F_FFFF => Region::Rom,
        // ROM disks
        0x00A0_0000..=0x00DF_FFFF => Region::Ignored,
        0x00E0_0000..=0x00EF_FFFF => Region::SlimRom,
        0x00F0_0000..=0x00F0_FFFF if ram15 => Region::RomOverRam,
        0x00F0_0000..=0x00F0_FFFF => Region::Normandy,
        0x00F1_0000..=0x00F3_FFFF if ram15 => Region::Rom,
        0x00F1_0000..=0x00F3_FFFF => Region::Ignored,
        0x00F6_0000..=0x00F6_FFFF => Region::Swim,
        0x00F7_0000..=0x00F7_FFFF => Region::Via,
        0x00F8_0000..=0x00F8_FFFF => Region::ExtRom,
        0x00F9_0000..=0x00F9_FFFF => Region::Scsi,
        0x00FA_0000..=0x00FA_FFFF => Region::Video,
        0x00FB_0000..=0x00FB_FFFF => Region::Asc,
        0x00FC_0000..=0x00FC_FFFF | 0x00FE_0000..=0x00FE_FFFF => Region::Normandy,
        0x00FD_0000..=0x00FD_FFFF => Region::Scc,
        _ => Region::None,
    }
}

fn build_map(model: MacModel) -> AddressMap {
    std::array::from_fn(|overlay| {
        std::array::from_fn(|block| {
            region(model, overlay == 1, (block as Address) << MAP_BLOCK_SHIFT)
        })
    })
}

fn empty_map() -> AddressMap {
    [[Region::None; MAP_BLOCKS]; 2]
}

impl<TRenderer> Debuggable for MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        let result = vec![
            dbgprop_nest!("Apple Sound Chip", self.asc),
            dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA (SY6522)", self.via),
            dbgprop_nest!("Power Manager", self.pmgr),
            dbgprop_nest!("Normandy", self.normandy),
        ];

        result
    }
}
