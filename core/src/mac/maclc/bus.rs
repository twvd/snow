use std::thread;
use std::time::{Duration, Instant};

use super::via2::Via2;
use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::mac::asc::Asc;
use crate::mac::scc::Scc;
use crate::mac::scsi::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::MacModel;
use crate::renderer::{AudioReceiver, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent};

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

pub struct MacLCBus<TRenderer: Renderer> {
    renderer: TRenderer,
    cycles: Ticks,

    /// The currently emulated Macintosh model
    model: MacModel,

    /// Trace non-ROM/RAM access
    pub trace: bool,

    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via1: Via,
    pub(crate) via2: Via2,
    pub(crate) scc: Scc,
    pub(crate) asc: Asc,
    via_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,

    ram_mask: usize,
    rom_mask: usize,

    overlay: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Last vblank time (for syncing to video)
    vblank_time: Instant,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,
}

impl<TRenderer> MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
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

    pub fn new(model: MacModel, rom: &[u8], renderer: TRenderer) -> Self {
        let ram_size = model.ram_size();

        let mut bus = Self {
            renderer,
            cycles: 0,
            model,
            trace: false,

            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            via1: Via::new(model),
            via2: Via2::new(model),
            via_clock: 0,
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), 16_000_000),
            scsi: ScsiController::new(),
            asc: Asc::default(),
            mouse_ready: false,

            ram_mask: (ram_size - 1),
            rom_mask: rom.len() - 1,

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            vblank_time: Instant::now(),
            progkey_pressed: LatchingEvent::default(),
        };

        // Disable memory test
        if let Some((addr, value)) = model.disable_memtest() {
            info!("Skipping memory test");
            bus.write_ram(addr, value);
        }

        bus
    }

    pub(crate) fn get_audio_channel(&self) -> AudioReceiver {
        self.asc.receiver.clone()
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

    fn write_overlay(&mut self, addr: Address, val: Byte) -> Option<()> {
        if self.trace {
            trace!("WRO {:08X} - {:02X}", addr, val);
        }

        match addr & 0xFF_FFFF {
            0xA0_0000..=0xDF_FFFF => {
                debug!("Overlay off");
                self.overlay = false;
                Some(())
            }
            _ => self.write_32bit(addr, val),
        }
    }

    fn write_32bit(&mut self, addr: Address, val: Byte) -> Option<()> {
        if self.trace && !(0x0000_0000..=0x003F_FFFF).contains(&addr) {
            trace!("WR {:08X} - {:02X}", addr, val);
        }

        if addr & (1 << 31) != 0 {
            // Expansion slot
            return None;
        }

        match addr & 0xFF_FFFF {
            // RAM
            0x00_0000..=0x9F_FFFF => {
                let idx = addr as usize & self.ram_mask;
                self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                Some(self.ram[idx] = val)
            }
            // VIA 1
            0xF0_0000..=0xF0_1FFF => self.via1.write(addr, val),
            // SCC
            0xF0_4000..=0xF0_5FFF => self.scc.write(addr >> 1, val),
            // SCSI
            0xF0_6000..=0xF0_7FFF => Some(self.scsi.write_dma(val)),
            0xF1_0000..=0xF1_1FFF => self.scsi.write(addr, val),
            0xF1_2000..=0xF1_2FFF => Some(self.scsi.write_dma(val)),
            // Sound
            0xF1_4000..=0xF1_5FFF => Some(()),
            // SWIM
            0xF1_6000..=0xF1_7FFF => self.swim.write(addr, val),
            // VIA 2
            0xF2_6000..=0xF2_7FFF => self.via2.write(addr, val),
            // Expansion area
            //0x0001_8000..=0x0001_FFFF => Some(()),
            _ => None,
        }
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr & 0xFF_FFFF {
            // ROM (overlay)
            0x00_0000..=0x9F_FFFF => {
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            0xA0_0000..=0xDF_FFFF => {
                log::debug!("Overlay off");
                self.overlay = false;
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            _ => self.read_32bit(addr),
        };
        if self.trace {
            trace!("RDO {:08X} - {:02X?}", addr, result);
        }

        result
    }

    fn read_32bit(&mut self, addr: Address) -> Option<Byte> {
        if addr & (1 << 31) != 0 {
            // Expansion slot
            return None;
        }

        let result = match addr & 0xFF_FFFF {
            // RAM
            0x00_0000..=0x9F_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            // ROM
            0xA0_0000..=0xDF_FFFF => {
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            // VIA 1
            0xF0_0000..=0xF0_1FFF => self.via1.read(addr),
            // SCC
            0xF0_4000..=0xF0_5FFF => self.scc.read(addr >> 1),
            // Sound
            0xF1_4000..=0xF1_5FFF => Some(0xFF),
            // SCSI
            0xF0_6000..=0xF0_7FFF => Some(self.scsi.read_dma()),
            0xF1_0000..=0xF1_1FFF => self.scsi.read(addr),
            0xF1_2000..=0xF1_2FFF => Some(self.scsi.read_dma()),
            // SWIM
            0xF1_6000..=0xF1_7FFF => self.swim.read(addr),
            // VIA 2
            0xF2_6000..=0xF2_7FFF => self.via2.read(addr),
            _ => None,
        };

        if self.trace && !(0x0000_0000..=0x3FFF_FFFF).contains(&addr) {
            trace!("RD {:08X} - {:02X?}", addr, result);
        }
        result
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, _button: Option<bool>) {
        let old_x = self.read_ram::<u16>(Self::ADDR_RAWMOUSE_X);
        let old_y = self.read_ram::<u16>(Self::ADDR_RAWMOUSE_Y);

        if !self.mouse_ready && (old_x != 15 || old_y != 15) {
            // Wait until the boot process has initialized the mouse position so we don't
            // interfere with the memory test.
            return;
        }
        self.mouse_ready = true;

        if relx != 0 || rely != 0 {
            let new_x = old_x.wrapping_add_signed(relx);
            let new_y = old_y.wrapping_add_signed(rely);

            // Report updated mouse coordinates to OS
            self.write_ram(Self::ADDR_MTEMP_X, new_x);
            self.write_ram(Self::ADDR_MTEMP_Y, new_y);
            if self.model >= MacModel::SE {
                // SE+ needs to see even a small difference between the current (RawMouse)
                // and new (MTemp) position, otherwise the change is ignored.
                self.write_ram(Self::ADDR_RAWMOUSE_X, new_x - 1);
                self.write_ram(Self::ADDR_RAWMOUSE_Y, new_y + 1);
            } else {
                self.write_ram(Self::ADDR_RAWMOUSE_X, new_x);
                self.write_ram(Self::ADDR_RAWMOUSE_Y, new_y);
            }
            self.write_ram(Self::ADDR_CRSRNEW, 1_u8);
        }
    }

    /// Updates the mouse position (absolute coordinates)
    pub fn mouse_update_abs(&mut self, x: u16, y: u16) {
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

    /// Tests for wait states on bus access
    fn in_waitstate(&self, _addr: Address) -> bool {
        // TODO
        false
    }

    /// Programmer's key pressed
    pub fn progkey(&mut self) {
        self.progkey_pressed.set();
    }

    pub fn video_blank(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<TRenderer> Bus<Address, Byte> for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_mask(&self) -> Address {
        0xFFFFFFFF
    }

    fn read(&mut self, addr: Address) -> BusResult<Byte> {
        if self.in_waitstate(addr) {
            return BusResult::WaitState;
        }

        let val = if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_32bit(addr)
        };

        if let Some(v) = val {
            BusResult::Ok(v)
        } else {
            warn!("Read from unimplemented address: {:08X}", addr);
            BusResult::Ok(0xFF)
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> BusResult<Byte> {
        if self.in_waitstate(addr) {
            return BusResult::WaitState;
        }

        let written = if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_32bit(addr, val)
        };

        // Sync values that live in multiple places
        self.swim.sel = self.via1.a_out.sel();
        self.swim.intdrive = self.via1.a_out.drivesel();

        if written.is_none() {
            warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
        }
        BusResult::Ok(val)
    }

    fn reset(&mut self) -> Result<()> {
        // Clear RAM
        self.ram.fill(0);

        // Disable memory test
        if let Some((addr, value)) = self.model.disable_memtest() {
            self.write_ram(addr, value);
        }

        // Take the ADB transceiver out because that contains crossbeam channels..
        let oldadb = std::mem::replace(&mut self.via1, Via::new(self.model)).adb;
        let _ = std::mem::replace(&mut self.via1.adb, oldadb);

        self.scc = Scc::new();

        self.overlay = true;
        Ok(())
    }
}

impl<TRenderer> Tickable for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // This is called from the CPU, at the CPU clock speed
        assert_eq!(ticks, 1);
        self.cycles += ticks;

        // The Mac II generates the VIA clock through some dividers on the logic board.
        // This same logic generates wait states when the VIAs are accessed.
        self.via_clock += ticks;
        while self.via_clock >= 20 {
            // TODO VIA wait states
            self.via_clock -= 20;

            self.via1.tick(1)?;
            self.via2.tick(1)?;
        }

        // Legacy VBlank interrupt
        if self.cycles % (16_000_000 / 60) == 0 {
            self.via1.ifr.set_vblank(true);

            if self.speed == EmulatorSpeed::Video || self.speed == EmulatorSpeed::Accurate {
                // Sync to 60 fps video
                let frametime = self.vblank_time.elapsed().as_micros() as u64;
                const DESIRED_FRAMETIME: u64 = 1_000_000 / 60;

                self.vblank_time = Instant::now();

                if frametime < DESIRED_FRAMETIME {
                    thread::sleep(Duration::from_micros(DESIRED_FRAMETIME - frametime));
                }
            }
        }

        self.via2.ifr.set_scsi_irq(self.scsi.get_irq());
        self.via2.ifr.set_scsi_drq(self.scsi.get_drq());

        self.swim.intdrive = self.via1.a_out.drivesel();
        self.swim.tick(1)?;

        Ok(1)
    }
}

impl<TRenderer> IrqSource for MacLCBus<TRenderer>
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
        if self.via2.ifr.0 & self.via2.ier.0 != 0 {
            return Some(2);
        }
        if self.via1.ifr.0 & self.via1.ier.0 != 0 {
            return Some(1);
        }

        None
    }
}

impl<TRenderer> InspectableBus<Address, Byte> for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        // Everything up to 0x4FFFFFFF is safe (RAM/ROM only)
        if addr >= 0x5000_0000 {
            None
        } else if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_32bit(addr)
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        // Everything up to 0x4FFFFFFF is safe (RAM/ROM only)
        if addr >= 0x5000_0000 {
            None
        } else if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_32bit(addr, val)
        }
    }
}

impl<TRenderer> Debuggable for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        let result = vec![
            dbgprop_nest!("Apple Desktop Bus", self.via1.adb),
            dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA 1 (V8)", self.via1),
            dbgprop_nest!("VIA 2 (V8)", self.via2),
        ];
        result
    }
}
