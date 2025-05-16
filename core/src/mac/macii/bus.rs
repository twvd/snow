use std::sync::atomic::Ordering;
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

use crate::mac::nubus::mdc12::Mdc12;
use anyhow::Result;
use bit_set::BitSet;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

pub struct MacIIBus<TRenderer: Renderer> {
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
    eclock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,

    ram_mask: usize,
    rom_mask: usize,

    overlay: bool,
    amu_active: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    // /// Last pushed audio sample
    //last_audiosample: u8,
    /// Last vblank time (for syncing to video)
    vblank_time: Instant,

    // /// VPA/E-clock sync in progress
    //vpa_sync: bool,
    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// NuBus cards (base address: $9)
    nubus_devices: [Option<Mdc12>; 6],
}

impl<TRenderer> MacIIBus<TRenderer>
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
            eclock: 0,
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd()),
            scsi: ScsiController::new(),
            asc: Asc::default(),
            mouse_ready: false,

            ram_mask: (ram_size - 1),
            rom_mask: rom.len() - 1,

            overlay: true,
            amu_active: false,
            speed: EmulatorSpeed::Accurate,
            //last_audiosample: 0,
            vblank_time: Instant::now(),
            //vpa_sync: false,
            progkey_pressed: LatchingEvent::default(),

            nubus_devices: [Some(Mdc12::new()), None, None, None, None, None],
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

        match addr {
            // 0x0000_0000 - 0x4FFF_FFFF is ROM
            0x5000_0000..=0xFFFF_FFFF => self.write_32bit(addr, val),
            _ => None,
        }
    }

    fn write_32bit(&mut self, addr: Address, val: Byte) -> Option<()> {
        if self.trace && !(0x0000_0000..=0x003F_FFFF).contains(&addr) {
            trace!("WR {:08X} - {:02X}", addr, val);
        }

        match addr {
            // RAM
            0x0000_0000..=0x4FFF_FFFF => {
                let idx = addr as usize & self.ram_mask;
                self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                Some(self.ram[idx] = val)
            }
            // I/O region (repeats)
            0x5000_0000..=0x51FF_FFFF => match addr & 0x1_FFFF {
                // VIA 1
                0x0000_0000..=0x0000_1FFF => self.via1.write(addr, val),
                // VIA 2
                0x0000_2000..=0x0000_3FFF => self.via2.write(addr, val),
                // SCC
                0x0000_4000..=0x0000_5FFF => self.scc.write(addr >> 1, val),
                // SCSI
                0x0000_6000..=0x0000_6003 => Some(self.scsi.write_dma(val)),
                0x0001_0000..=0x0001_1FFF => self.scsi.write(addr, val),
                0x0001_2000..=0x0001_2FFF => Some(self.scsi.write_dma(val)),
                // Sound
                0x0001_4000..=0x0001_5FFF => Some(()),
                // IWM
                0x0001_6000..=0x0001_7FFF => self.swim.write(addr, val),
                // Expansion area
                //0x0001_8000..=0x0001_FFFF => Some(()),
                _ => None,
            },
            // NuBus super slot
            0x6000_0000..=0xEFFF_FFFF => None,
            // NuBus standard slot
            0xF100_0000..=0xFFFF_FFFF => {
                let nubus_addr = (addr >> 24) & 0x0F;
                if nubus_addr < 0x09 || nubus_addr == 0x0F {
                    None
                } else if let Some(dev) = self.nubus_devices[(nubus_addr - 0x09) as usize].as_mut()
                {
                    dev.write(addr & 0xFF_FFFF, val)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn write_24bit(&mut self, addr: Address, val: Byte) -> Option<()> {
        self.write_32bit(self.amu_translate(addr), val)
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            // ROM
            0x0000_0000..=0x4FFF_FFFF => {
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            0x5000_0000..=0xFFFF_FFFF => self.read_32bit(addr),
        };
        if self.trace {
            trace!("RDO {:08X} - {:02X?}", addr, result);
        }

        result
    }

    fn read_24bit(&mut self, addr: Address) -> Option<Byte> {
        self.read_32bit(self.amu_translate(addr))
    }

    fn read_32bit(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            // RAM
            0x0000_0000..=0x3FFF_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            // ROM
            0x4000_0000..=0x4FFF_FFFF => {
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            // I/O region (repeats)
            0x5000_0000..=0x51FF_FFFF => match addr & 0x1_FFFF {
                // VIA 1
                0x0000_0000..=0x0000_1FFF => self.via1.read(addr),
                // VIA 2
                0x0000_2000..=0x0000_3FFF => self.via2.read(addr),
                // SCC
                0x0000_4000..=0x0000_5FFF => self.scc.read(addr >> 1),
                // SCSI
                0x0000_6060..=0x0000_6063 => Some(self.scsi.read_dma()),
                0x0001_0000..=0x0001_1FFF => self.scsi.read(addr),
                0x0001_2000..=0x0001_2FFF => Some(self.scsi.read_dma()),
                // Sound
                0x0001_4000..=0x0001_5FFF => Some(0),
                // IWM
                0x0001_6000..=0x0001_7FFF => self.swim.read(addr),
                // Expansion area
                //0x0001_8000..=0x0001_FFFF => Some(0xFF),
                _ => None,
            },
            // NuBus super slot
            0x6000_0000..=0xEFFF_FFFF => None,
            // NuBus standard slot
            0xF100_0000..=0xFFFF_FFFF => {
                let nubus_addr = (addr >> 24) & 0x0F;
                if nubus_addr < 0x09 || nubus_addr == 0x0F {
                    None
                } else if let Some(dev) = self.nubus_devices[(nubus_addr - 0x09) as usize].as_mut()
                {
                    dev.read(addr & 0xFF_FFFF)
                } else {
                    None
                }
            }
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

    fn amu_translate(&self, addr: Address) -> Address {
        match addr & 0xFFFFFF {
            0x00_0000..=0x7F_FFFF => addr & 0x7F_FFFF,
            0x80_0000..=0x8F_FFFF => 0x4000_0000 | (addr & 0xF_FFFF),
            0x90_0000..=0x9F_FFFF => 0xF900_0000 | (addr & 0xF_FFFF),
            0xA0_0000..=0xAF_FFFF => 0xFA00_0000 | (addr & 0xF_FFFF),
            0xB0_0000..=0xBF_FFFF => 0xFB00_0000 | (addr & 0xF_FFFF),
            0xC0_0000..=0xCF_FFFF => 0xFC00_0000 | (addr & 0xF_FFFF),
            0xD0_0000..=0xDF_FFFF => 0xFD00_0000 | (addr & 0xF_FFFF),
            0xE0_0000..=0xEF_FFFF => 0xFE00_0000 | (addr & 0xF_FFFF),
            0xF0_0000..=0xFF_FFFF => 0x5000_0000 | (addr & 0xF_FFFF),
            _ => unreachable!(),
        }
    }

    /// Prepares the image and sends it to the frontend renderer
    fn render(&mut self) -> Result<()> {
        let fb = &self.nubus_devices[0].as_ref().unwrap().vram;

        let buf = self.renderer.get_buffer();
        for idx in 0..(self.model.display_width() as usize * self.model.display_height() as usize) {
            let byte = idx / 8;
            let bit = idx % 8;
            if fb[0x80 + byte] & (1 << (7 - bit)) == 0 {
                buf[idx * 4].store(0xEE, Ordering::Release);
                buf[idx * 4 + 1].store(0xEE, Ordering::Release);
                buf[idx * 4 + 2].store(0xEE, Ordering::Release);
            } else {
                buf[idx * 4].store(0x22, Ordering::Release);
                buf[idx * 4 + 1].store(0x22, Ordering::Release);
                buf[idx * 4 + 2].store(0x22, Ordering::Release);
            }
            buf[idx * 4 + 3].store(0xFF, Ordering::Release);
        }
        self.renderer.update()?;

        Ok(())
    }
}

impl<TRenderer> Bus<Address, Byte> for MacIIBus<TRenderer>
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

        let val = if self.amu_active {
            self.read_24bit(addr)
        } else if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_32bit(addr)
        };

        if let Some(v) = val {
            BusResult::Ok(v)
        } else {
            if self.amu_active {
                warn!(
                    "Read from unimplemented address: {:06X} -> {:08X}",
                    addr & 0xFFFFFF,
                    self.amu_translate(addr),
                );
            } else {
                warn!("Read from unimplemented address: {:08X}", addr);
            }
            BusResult::Ok(0xFF)
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> BusResult<Byte> {
        if self.in_waitstate(addr) {
            return BusResult::WaitState;
        }

        let written = if self.amu_active {
            self.write_24bit(addr, val)
        } else if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_32bit(addr, val)
        };

        if self.overlay && !self.via1.a_out.overlay() {
            debug!("Overlay off");
            self.overlay = false;
        }

        // Sync values that live in multiple places
        self.swim.sel = self.via1.a_out.sel();

        if written.is_none() {
            if self.amu_active {
                warn!(
                    "Write to unimplemented address: {:06X} -> {:08X} {:02X}",
                    addr & 0xFFFFFF,
                    self.amu_translate(addr),
                    val
                );
            } else {
                warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
            }
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
        self.via2.b_out.set_vfc3(false);
        self.via2.ddrb.set_vfc3(false);
        self.amu_active = false;
        Ok(())
    }
}

impl<TRenderer> Tickable for MacIIBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // This is called from the CPU, at the CPU clock speed
        assert_eq!(ticks, 1);
        self.cycles += ticks;

        self.amu_active = self.via2.ddrb.vfc3() && !self.via2.b_out.vfc3();

        self.eclock += ticks;
        while self.eclock >= 20 {
            // The E Clock is roughly 1/10th of the CPU clock
            // TODO ticks when VPA is asserted
            self.eclock -= 20;

            self.via1.tick(1)?;
            self.via2.tick(1)?;
        }

        // VBlank interrupt
        if self.nubus_devices[0].as_mut().unwrap().render.get_clear() {
            self.render()?;
            self.via1.ifr.set_vblank(true);

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

        // NuBus slot IRQs and ticks
        let mut slot_irqs = 0;
        for slot in 0..(self.nubus_devices.len()) {
            if let Some(dev) = self.nubus_devices[slot].as_mut() {
                dev.tick(ticks)?;
                if dev.get_irq() {
                    slot_irqs |= 1 << slot;
                }
            }
        }
        self.via2.a_in.set_v2irqs(!slot_irqs);
        if slot_irqs > 0 {
            self.via2.ifr.set_slot(true);
        }
        self.via2.ifr.set_scsi_irq(self.scsi.get_irq());
        self.via2.ifr.set_scsi_drq(self.scsi.get_drq());

        self.swim.tick(1)?;

        Ok(1)
    }
}

impl<TRenderer> IrqSource for MacIIBus<TRenderer>
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

impl<TRenderer> InspectableBus<Address, Byte> for MacIIBus<TRenderer>
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

impl<TRenderer> Debuggable for MacIIBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        vec![
            dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA 1 (SY6522)", self.via1),
            dbgprop_nest!("VIA 2 (SY6522)", self.via2),
        ]
    }
}
