use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::macii::bus::{DEFAULT_BUS_SPEED, RAM_DIRTY_PAGESIZE};
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::renderer::{AudioProvider, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent};

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct MacLCBus<TRenderer: Renderer> {
    cycles: Ticks,

    /// The currently emulated Macintosh model
    model: MacModel,

    rom_mask: usize,
    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    #[serde(skip)]
    renderer: Option<TRenderer>,

    pub(crate) scc: Scc,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    rtc: Rtc,

    overlay: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    bus_frequency: Ticks,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer> MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    /// The V8 only decodes A31 and A23-A0. A31 selects the expansion slot,
    /// which is not implemented.
    const ADDR_MASK: Address = 0x00FF_FFFF;

    /// Value to return on open bus
    const OPENBUS: u8 = 0xFF;

    pub fn new(
        model: MacModel,
        rom: &[u8],
        renderer: TRenderer,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());

        Self {
            cycles: 0,
            model,

            rom_mask: rom.len() - 1,
            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            renderer: Some(renderer),
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), DEFAULT_BUS_SPEED),
            scsi: ScsiController::new(),
            rtc: Rtc::default(),

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            bus_frequency: DEFAULT_BUS_SPEED,
            progkey_pressed: LatchingEvent::default(),
            mouse_mode,
        }
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        self.renderer = Some(renderer);

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.rtc.effective_speed()
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        self.scsi.set_audio_provider(provider)
    }

    fn read_rom(&self, addr: Address) -> Option<Byte> {
        Some(
            *self
                .rom
                .get(addr as usize & self.rom_mask)
                .unwrap_or(&Self::OPENBUS),
        )
    }

    /// Wraps a device read result in a mapped region: logs a warning if the
    /// device did not handle the address. The enclosing region is considered
    /// mapped, so this never triggers a bus error.
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

    fn write_overlay(&mut self, addr: Address, val: Byte) -> Option<()> {
        self.write_normal(addr, val)
    }

    fn write_normal(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // RAM
            0x00_0000..=0x9F_FFFF => {
                let idx = addr as usize;
                if idx < self.ram.len() {
                    self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                    self.ram[idx] = val;
                }
                Some(())
            }
            // ROM
            0xA0_0000..=0xAF_FFFF => Some(()),
            // VIA 1
            0xF0_0000..=0xF0_1FFF => Self::dev_write(addr, val, None),
            // SCC
            0xF0_4000..=0xF0_5FFF => Self::dev_write(addr, val, self.scc.write(addr >> 1, val)),
            // SCSI
            0xF0_6000..=0xF0_7FFF => Some(self.scsi.write_dma(val)),
            0xF1_0000..=0xF1_1FFF => Self::dev_write(addr, val, self.scsi.write(addr, val)),
            0xF1_2000..=0xF1_3FFF => Some(self.scsi.write_dma(val)),
            // ASC (sound)
            0xF1_4000..=0xF1_5FFF => Self::dev_write(addr, val, None),
            // SWIM
            0xF1_6000..=0xF1_7FFF => Self::dev_write(addr, val, self.swim.write(addr, val)),
            // RAMDAC
            0xF2_4000..=0xF2_5FFF => Self::dev_write(addr, val, None),
            // V8 registers
            0xF2_6000..=0xF2_7FFF => Self::dev_write(addr, val, None),
            // Video RAM
            0xF4_0000..=0xFB_FFFF => Self::dev_write(addr, val, None),
            _ => None,
        }
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // ROM (overlay)
            0x00_0000..=0x9F_FFFF => self.read_rom(addr),
            // ROM (disables overlay)
            0xA0_0000..=0xAF_FFFF => {
                self.overlay = false;
                self.read_rom(addr)
            }
            _ => self.read_normal(addr),
        }
    }

    fn read_normal(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // RAM
            0x00_0000..=0x9F_FFFF => Some(*self.ram.get(addr as usize).unwrap_or(&Self::OPENBUS)),
            // ROM
            0xA0_0000..=0xAF_FFFF => self.read_rom(addr),
            // VIA 1
            0xF0_0000..=0xF0_1FFF => Some(Self::dev_read(addr, None)),
            // SCC
            0xF0_4000..=0xF0_5FFF => Some(Self::dev_read(addr, self.scc.read(addr >> 1))),
            // SCSI
            0xF0_6000..=0xF0_7FFF => Some(self.scsi.read_dma()),
            0xF1_0000..=0xF1_1FFF => Some(Self::dev_read(addr, self.scsi.read(addr))),
            0xF1_2000..=0xF1_3FFF => Some(self.scsi.read_dma()),
            // ASC (sound)
            0xF1_4000..=0xF1_5FFF => Some(Self::dev_read(addr, None)),
            // SWIM
            0xF1_6000..=0xF1_7FFF => Some(Self::dev_read(addr, self.swim.read(addr))),
            // RAMDAC
            0xF2_4000..=0xF2_5FFF => Some(Self::dev_read(addr, None)),
            // V8 registers
            0xF2_6000..=0xF2_7FFF => Some(Self::dev_read(addr, None)),
            // Video RAM
            0xF4_0000..=0xFB_FFFF => Some(Self::dev_read(addr, None)),
            _ => None,
        }
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, _relx: i16, _rely: i16, _button: Option<bool>) {}

    /// Updates the mouse position (absolute coordinates)
    pub fn mouse_update_abs(&mut self, _x: u16, _y: u16) {}

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

    /// Programmer's key pressed
    pub fn progkey(&mut self) {
        self.progkey_pressed.set();
    }

    pub fn video_blank(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn keyboard_event(&mut self, _ke: KeyEvent) {}

    pub fn input_release_all(&mut self) {}

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.rtc
    }
}

impl<TRenderer> Bus<Address, Byte> for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn read(&mut self, addr: Address) -> BusResult<Byte> {
        let addr = addr & Self::ADDR_MASK;
        let val = if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        };

        match val {
            Some(v) => BusResult::Ok(v),
            None => {
                warn!("Bus error (read) at unmapped address: {:08X}", addr);
                BusResult::BusError
            }
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> BusResult<Byte> {
        let addr = addr & Self::ADDR_MASK;
        let written = if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        };

        if written.is_none() {
            warn!(
                "Bus error (write) at unmapped address: {:08X} {:02X}",
                addr, val
            );
            return BusResult::BusError;
        }
        BusResult::Ok(val)
    }

    fn get_mask(&self) -> Address {
        0xFFFFFFFF
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            // Clear RAM
            self.ram.fill(0);
            self.ram_dirty
                .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
        }

        self.scc = Scc::new();
        self.overlay = true;
        Ok(false)
    }
}

impl<TRenderer> Tickable for MacLCBus<TRenderer>
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

        self.scsi.tick(ticks, ctx)?;
        self.swim.tick(ticks, ())?;

        Ok(ticks)
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
        None
    }
}

impl<TRenderer> InspectableBus<Address, Byte> for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        let addr = addr & Self::ADDR_MASK;
        match addr {
            0x00_0000..=0x9F_FFFF if self.overlay => self.read_rom(addr),
            0x00_0000..=0xAF_FFFF => self.read_normal(addr),
            _ => None,
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        let addr = addr & Self::ADDR_MASK;
        match addr {
            0x00_0000..=0xAF_FFFF => self.write_normal(addr, val),
            _ => None,
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

        vec![
            dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi),
            dbgprop_nest!("SWIM", self.swim),
        ]
    }
}
