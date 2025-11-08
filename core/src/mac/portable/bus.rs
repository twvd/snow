use std::thread;
use std::time::{Duration, Instant};

use super::normandy::Normandy;
use super::pmgr::Pmgr;
use super::via::Via;
use super::video::Video;
use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::MouseMode;
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::asc::Asc;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::MacModel;
use crate::renderer::{AudioReceiver, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};
use serde::{Deserialize, Serialize};

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

pub const CLOCK_SPEED: Ticks = 16_000_000;

const IDLE_DTACK_DELAY: u8 = 64;
const SLIM_DTACK_DELAY: u8 = 16;

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
    pub(crate) scc: Scc,
    pub(crate) video: Video<TRenderer>,
    pub(crate) asc: Asc,
    via_clock: Ticks,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    pub(crate) pmgr: Pmgr,
    normandy: Normandy,

    overlay: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Last vblank time (for syncing to video)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for in Video speed mode.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer: Renderer> MacPortableBus<TRenderer>
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
            log::info!("Extension ROM present");
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
                ])
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
            video: Video::new(renderer),
            via_clock: 0,
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), 16_000_000),
            scsi: ScsiController::new(),
            asc: Asc::default(),
            mouse_ready: false,

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            vblank_time: Instant::now(),
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
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        self.asc.after_deserialize();

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / crate::mac::compact::bus::RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub(crate) fn get_audio_channel(&self) -> AudioReceiver {
        self.asc.receiver.as_ref().unwrap().clone()
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

    fn write_overlay(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // ROM (disables overlay
            0x0090_0000..=0x009F_FFFF => {
                self.overlay = false;
                self.write_normal(addr, val)
            }
            _ => self.write_normal(addr, val),
        }
    }

    fn write_normal(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // RAM
            0x0000_0000..=0x008F_FFFF => self.normal_ram_write(addr, val),
            // ROM, or Remapped RAM for 15MB RAM Mod
            0x0090_0000..=0x009F_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_write(addr, val)
                } else {
                    Some(())
                }
            }
            // ROM Disks, or Remapped RAM for 15MB RAM Mod
            0x00A0_0000..=0x00DF_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_write(addr, val)
                } else {
                    Some(())
                }
            }
            // SLIM ROM, or Remapped RAM for 15MB RAM Mod
            0x00E0_0000..=0x00EF_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_write(addr, val)
                } else {
                    Some(())
                }
            }
            // SLIM/Normandy, or Remapped ROM for 15MB RAM Mod
            0x00F0_0000..=0x00F0_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_write(addr, val)
                } else {
                    self.normandy.write(addr, val)
                }
            }
            // Remapped ROM for 15MB RAM Mod
            0x00F1_0000..=0x00F3_FFFF => Some(()),
            // SWIM
            0x00F6_0000..=0x00F6_FFFF => self.swim.write(addr, val),
            // VIA
            0x00F7_0000..=0x00F7_FFFF => self.via.write(addr, val),
            // SCSI
            0x00F9_0000..=0x00F9_FFFF => self.scsi.write(addr, val),
            // Video
            0x00FA_0000..=0x00FA_FFFF => {
                let offset = (addr & 0x7FFF) as usize;
                Some(self.video.framebuffer[offset] = val)
            }
            // Sound
            0x00FB_0000..=0x00FB_FFFF => self.asc.write(addr & 0xFFF, val),
            // Normandy registers
            0x00FC_0000..=0x00FC_FFFF => self.normandy.write(addr, val),
            // SCC
            0x00FD_0000..=0x00FD_FFFF => self.scc.write(addr >> 1, val),
            // Normandy registers
            0x00FE_0000..=0x00FE_FFFF => self.normandy.write(addr, val),
            _ => None,
        }
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            0x0000_0000..=0x000F_FFFF => {
                Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
            }
            0x0090_0000..=0x009F_FFFF => {
                self.overlay = false;
                self.read_normal(addr)
            }
            _ => self.read_normal(addr),
        };
        result
    }

    fn read_normal(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            0x0000_0000..=0x008F_FFFF => self.normal_ram_read(addr),
            // ROM, or Remapped RAM for 15MB RAM Mod
            0x0090_0000..=0x009F_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_read(addr)
                } else {
                    Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
                }
            }
            // ROM Disks, or Remapped RAM for 15MB RAM Mod
            0x00A0_0000..=0x00DF_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_read(addr)
                } else {
                    None
                }
            }
            // SLIM ROM, or Remapped RAM for 15MB RAM Mod
            0x00E0_0000..=0x00EF_FFFF => {
                if self.model == MacModel::Portable15MB {
                    self.normal_ram_read(addr)
                } else {
                    self.normandy.read(addr)
                }
            }
            // SLIM/Normandy, or Remapped ROM for 15MB RAM Mod
            0x00F0_0000..=0x00F0_FFFF => {
                if self.model == MacModel::Portable15MB {
                    Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
                } else {
                    self.normandy.read(addr)
                }
            }
            // Remapped ROM for 15MB RAM Mod
            0x00F1_0000..=0x00F3_FFFF => {
                if self.model == MacModel::Portable15MB {
                    Some(*self.rom.get(addr as usize & self.rom_mask).unwrap_or(&0xFF))
                } else {
                    None
                }
            }
            // SWIM
            0x00F6_0000..=0x00F6_FFFF => self.swim.read(addr),
            // VIA
            0x00F7_0000..=0x00F7_FFFF => self.via.read(addr),
            // Test software region / extension ROM
            0x00F8_0000..=0x00F8_FFFF => Some(
                *self
                    .extension_rom
                    .get((addr - 0xF8_0000) as usize)
                    .unwrap_or(&0xFF),
            ),
            // SCSI
            0x00F9_0000..=0x00F9_FFFF => self.scsi.read(addr),
            // Video
            0x00FA_0000..=0x00FA_FFFF => {
                let offset = (addr & 0x7FFF) as usize;
                Some(self.video.framebuffer[offset])
            }
            // Sound
            0x00FB_0000..=0x00FB_FFFF => self.asc.read(addr & 0xFFF),
            // Normandy registers
            0x00FC_0000..=0x00FC_FFFF => self.normandy.read(addr),
            // SCC
            0x00FD_0000..=0x00FD_FFFF => self.scc.read(addr >> 1),
            // Normandy registers
            0x00FE_0000..=0x00FE_FFFF => self.normandy.read(addr),
            _ => None,
        };
        result
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
            }))
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
        self.pmgr.adb_event(&AdbEvent::Mouse(MouseEvent {
            button: None,
            rel_movement: None,
        }));

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

    /// Tests for wait states on bus access
    fn in_waitstate(&mut self, addr: Address) -> bool {
        match addr {
            0x0000_0000..=0x008F_FFFF => {
                if self.normandy.idle_speed {
                    match self.normandy.dtack_counter {
                        0 => {
                            self.normandy.dtack_counter = IDLE_DTACK_DELAY;
                            true
                        }
                        1 => {
                            self.normandy.dtack_counter -= 1;
                            false
                        }
                        _ => {
                            self.normandy.dtack_counter -= 1;
                            true
                        }
                    }
                } else if !self.normandy.slim_dtack & (0x0050_0000..=0x008F_FFFF).contains(&addr) {
                    match self.normandy.dtack_counter {
                        0 => {
                            self.normandy.dtack_counter = SLIM_DTACK_DELAY;
                            true
                        }
                        1 => {
                            self.normandy.dtack_counter -= 1;
                            false
                        }
                        _ => {
                            self.normandy.dtack_counter -= 1;
                            true
                        }
                    }
                } else {
                    false
                }
            }
            _ => false,
        }
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

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.pmgr.rtc
    }
}

impl<TRenderer> Bus<Address, Byte> for MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    fn read(&mut self, addr: Address) -> BusResult<Byte> {
        if self.in_waitstate(addr) {
            return BusResult::WaitState;
        }

        let val = if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        };

        if let Some(v) = val {
            BusResult::Ok(v)
        } else {
            warn!("Read from unimplemented address: {:08X}", addr);
            BusResult::Ok(Self::OPENBUS)
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> BusResult<Byte> {
        if self.in_waitstate(addr) {
            return BusResult::WaitState;
        }

        let written = if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        };

        // Sync values that live in multiple places
        self.swim.sel = self.via.b_out.headsel();

        if written.is_none() {
            warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
        }
        BusResult::Ok(val)
    }

    fn reset(&mut self, hard: bool) -> Result<()> {
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
        Ok(())
    }
}

impl<TRenderer> Tickable for MacPortableBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // This is called from the CPU, at the CPU clock speed
        assert_eq!(ticks, 1);
        self.cycles += ticks;

        self.via_clock += ticks;
        while self.via_clock >= 20 {
            // TODO VIA wait states
            self.via_clock -= 20;

            self.via.tick(1)?;
        }

        self.video.tick(1)?;

        // Legacy VBlank interrupt
        if self.cycles % (CLOCK_SPEED / 60) == 0 {
            self.via.ifr.set_vblank(true);

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

        if self.cycles % (CLOCK_SPEED / self.asc.sample_rate()) == 0 {
            self.asc.tick(self.speed == EmulatorSpeed::Accurate)?;
        }

        self.swim.intdrive = self.via.b_out.drivesel();
        self.swim.tick(1)?;

        self.pmgr.a_out = self.via.a_out.0;
        self.pmgr.a_in = self.via.a_in.0;
        self.pmgr.pmreq = self.via.b_out.pmreq();
        self.pmgr.onesec = self.via.ifr.onesec();
        self.pmgr.tick(1)?;
        self.via.b_in.set_pmack(self.pmgr.pmack);
        self.via.a_in.0 = self.pmgr.a_in;
        self.via.ifr.set_pmgr(self.pmgr.interrupt);

        Ok(1)
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
        if self.via.ifr.0 & self.via.ier.0 != 0 {
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
            None
        } else if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        if addr >= 0x00F0_0000 {
            None
        } else if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        }
    }
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
