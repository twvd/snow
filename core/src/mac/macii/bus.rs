use std::thread;
use std::time::{Duration, Instant};

use super::via2::Via2;
use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::MouseMode;
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::asc::Asc;
use crate::mac::nubus::mdc12::Mdc12;
use crate::mac::nubus::se30video::SE30Video;
use crate::mac::nubus::NubusCard;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::{MacModel, MacMonitor};
use crate::renderer::{AudioReceiver, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};
use serde::{Deserialize, Serialize};

/// Macintosh II main clock speed
pub const CLOCK_SPEED: Ticks = 16_000_000;

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

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct MacIIBus<TRenderer: Renderer, const AMU: bool> {
    cycles: Ticks,

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
    via_clock: Ticks,
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

    /// Last vblank time (for syncing to video)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for in Video speed mode.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,

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
        videorom: &[u8],
        extension_rom: Option<&[u8]>,
        mut renderers: Vec<TRenderer>,
        monitor: MacMonitor,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
        let ram_config = Self::RAM_CONFIG
            .iter()
            .find(|c| c.size == ram_size)
            .unwrap_or_else(|| panic!("Unsupported RAM size: {}", ram_size));

        if extension_rom.is_some() {
            log::info!("Extension ROM present");
        }

        let mut bus = Self {
            cycles: 0,
            model,

            rom: Vec::from(rom),
            extension_rom: extension_rom.map(Vec::from).unwrap_or_default(),
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

            ram_mask: usize::MAX,
            ram_mirror: ram_config.mirror,
            ram_expected_ramsiz: ram_config.expected_sz,
            rom_mask: rom.len() - 1,

            overlay: true,
            amu_active: false,
            speed: EmulatorSpeed::Accurate,
            //last_audiosample: 0,
            vblank_time: Instant::now(),
            //vpa_sync: false,
            progkey_pressed: LatchingEvent::default(),

            nubus_devices: if model == MacModel::SE30 {
                [
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(NubusCard::SE30Video(SE30Video::new(
                        videorom,
                        renderers.pop().unwrap(),
                    ))),
                ]
            } else {
                core::array::from_fn(|_| {
                    renderers
                        .pop()
                        .map(|r| NubusCard::MDC12(Mdc12::new(videorom, r, monitor)))
                })
            },
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
        if let Some(NubusCard::MDC12(c)) = self.nubus_devices[0].as_mut() {
            c.renderer = Some(renderer);
            // Make sure we have at least the last frame available
            c.render().unwrap();
        } else if let Some(NubusCard::SE30Video(c)) = self.nubus_devices[5].as_mut() {
            c.renderer = Some(renderer);
            // Make sure we have at least the last frame available
            c.render().unwrap();
        }

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
        match addr {
            // 0x0000_0000 - 0x4FFF_FFFF is ROM
            0x5000_0000..=0xFFFF_FFFF => self.write_32bit(addr, val),
            _ => None,
        }
    }

    fn write_32bit(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // RAM
            0x0000_0000..=0x3FFF_FFFF => {
                let idx = addr as usize & self.ram_mask;
                if idx >= self.ram.len() {
                    // Ignore silently to avoid a lot of spam during memory tests
                    Some(())
                } else {
                    self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                    Some(self.ram[idx] = val)
                }
            }
            // ROM
            0x4000_0000..=0x4FFF_FFFF => Some(()),
            // I/O region (repeats)
            0x5000_0000..=0x51FF_FFFF => match addr & 0x1_FFFF {
                // VIA 1
                0x0000_0000..=0x0000_1FFF if self.model == MacModel::SE30 => {
                    let result = self.via1.write(addr, val);
                    let Some(NubusCard::SE30Video(d)) = self.nubus_devices[5].as_mut() else {
                        unreachable!()
                    };
                    d.vblank_enable = !self.via1.b_out.se30_vblank_enable();
                    d.fb_select = self.via1.a_out.page2();
                    result
                }
                0x0000_0000..=0x0000_1FFF => self.via1.write(addr, val),
                // VIA 2
                0x0000_2000..=0x0000_3FFF => {
                    let result = self.via2.write(addr, val);

                    // Lazy update ramsize
                    if self.via2.a_out.v2ram0() == self.ram_expected_ramsiz {
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

                    result
                }
                // SCC
                0x0000_4000..=0x0000_5FFF => self.scc.write(addr >> 1, val),
                // SCSI
                0x0000_6000..=0x0000_6003 => Some(self.scsi.write_dma(val)),
                0x0001_0000..=0x0001_1FFF => self.scsi.write(addr, val),
                0x0001_2000..=0x0001_2FFF => Some(self.scsi.write_dma(val)),
                // ASC (sound)
                0x0001_4000..=0x0001_5FFF => self.asc.write(addr & 0xFFF, val),
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
                    match dev {
                        NubusCard::MDC12(dev) => dev.write(addr & 0xFF_FFFF, val),
                        NubusCard::SE30Video(dev) => dev.write(addr & 0xFF_FFFF, val),
                    }
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
        match addr {
            // ROM
            0x0000_0000..=0x4FFF_FFFF => Some(
                *self
                    .rom
                    .get(addr as usize & self.rom_mask)
                    .unwrap_or(&Self::OPENBUS),
            ),
            0x5000_0000..=0xFFFF_FFFF => self.read_32bit(addr),
        }
    }

    fn read_24bit(&mut self, addr: Address) -> Option<Byte> {
        self.read_32bit(self.amu_translate(addr))
    }

    fn read_32bit(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // RAM
            0x0000_0000..=0x3FFF_FFFF => {
                let idx = addr as usize & self.ram_mask;
                if idx >= self.ram.len() {
                    // Ignore silently to avoid a lot of spam during memory tests
                    Some(Self::OPENBUS)
                } else {
                    Some(self.ram[idx])
                }
            }
            // ROM
            0x4000_0000..=0x4FFF_FFFF => Some(
                *self
                    .rom
                    .get(addr as usize & self.rom_mask)
                    .unwrap_or(&Self::OPENBUS),
            ),
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
                // ASC (sound)
                0x0001_4000..=0x0001_5FFF => self.asc.read(addr & 0xFFF),
                // IWM
                0x0001_6000..=0x0001_7FFF => self.swim.read(addr),
                // Expansion area
                //0x0001_8000..=0x0001_FFFF => Some(Self::OPENBUS),
                _ => None,
            },
            // Extension ROM / test area
            0x5800_0000..=0x5FFF_FFFF => Some(
                *self
                    .extension_rom
                    .get((addr - 0x5800_0000) as usize)
                    .unwrap_or(&Self::OPENBUS),
            ),
            // NuBus super slot
            0x6000_0000..=0xEFFF_FFFF => None,
            // NuBus standard slot
            0xF100_0000..=0xFFFF_FFFF => {
                let nubus_addr = (addr >> 24) & 0x0F;
                if nubus_addr < 0x09 || nubus_addr == 0x0F {
                    None
                } else if let Some(dev) = self.nubus_devices[(nubus_addr - 0x09) as usize].as_mut()
                {
                    match dev {
                        NubusCard::MDC12(dev) => dev.read(addr & 0xFF_FFFF),
                        NubusCard::SE30Video(dev) => dev.read(addr & 0xFF_FFFF),
                    }
                } else {
                    None
                }
            }
            _ => None,
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
            match d {
                NubusCard::MDC12(d) => d.blank()?,
                NubusCard::SE30Video(d) => d.blank()?,
            }
        }
        Ok(())
    }

    /// Dispatches a key event to the keyboard
    pub fn keyboard_event(&mut self, ke: KeyEvent) {
        self.via1.adb.event(&AdbEvent::Key(ke));
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.via1.rtc
    }
}

impl<TRenderer, const AMU: bool> Bus<Address, Byte> for MacIIBus<TRenderer, AMU>
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

        let val = if AMU && self.amu_active {
            self.read_24bit(addr)
        } else if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_32bit(addr)
        };

        if let Some(v) = val {
            BusResult::Ok(v)
        } else {
            if AMU && self.amu_active {
                warn!(
                    "Read from unimplemented address: {:06X} -> {:08X}",
                    addr & 0xFFFFFF,
                    self.amu_translate(addr),
                );
            } else {
                warn!("Read from unimplemented address: {:08X}", addr);
            }
            BusResult::Ok(Self::OPENBUS)
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> BusResult<Byte> {
        if self.in_waitstate(addr) {
            return BusResult::WaitState;
        }

        let written = if AMU && self.amu_active {
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
        self.swim.intdrive = self.via1.a_out.drivesel();

        if written.is_none() {
            if AMU && self.amu_active {
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

    fn reset(&mut self, hard: bool) -> Result<()> {
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
        for d in self.nubus_devices.iter_mut().filter_map(|f| f.as_mut()) {
            d.reset();
        }
        self.asc.reset();

        self.amu_active = false;
        self.mouse_ready = false;
        self.overlay = true;
        Ok(())
    }
}

impl<TRenderer, const AMU: bool> Tickable for MacIIBus<TRenderer, AMU>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // This is called from the CPU, at the CPU clock speed
        assert_eq!(ticks, 1);
        self.cycles += ticks;

        if AMU {
            self.amu_active = self.via2.ddrb.vfc3() && !self.via2.b_out.vfc3();
        }

        self.scsi.tick(ticks)?;

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
        if self.cycles.is_multiple_of(CLOCK_SPEED / 60) {
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

        // Audio
        if self.asc.get_irq() {
            self.via2.ifr.set_asc(true);
        }
        if self
            .cycles
            .is_multiple_of(CLOCK_SPEED / self.asc.sample_rate())
        {
            self.asc.tick(self.speed == EmulatorSpeed::Accurate)?;
        }

        // NuBus slot IRQs and ticks
        let mut slot_irqs = 0;
        for (slot, dev) in self
            .nubus_devices
            .iter_mut()
            .enumerate()
            .filter_map(|(i, o)| o.as_mut().map(|d| (i, d)))
        {
            dev.tick(ticks)?;
            if dev.get_irq() {
                slot_irqs |= 1 << slot;
            }
        }
        self.via2.a_in.set_v2irqs(!slot_irqs);
        if slot_irqs > 0 {
            self.via2.ifr.set_slot(true);
        }
        self.via2.ifr.set_scsi_irq(self.scsi.get_irq());
        self.via2.ifr.set_scsi_drq(self.scsi.get_drq());

        self.swim.intdrive = self.via1.a_out.drivesel();
        self.swim.tick(1)?;

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
        if self.via2.ifr.0 & self.via2.ier.0 != 0 {
            return Some(2);
        }
        if self.via1.ifr.0 & self.via1.ier.0 != 0 {
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
