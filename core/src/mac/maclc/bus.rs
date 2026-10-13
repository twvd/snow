use super::pseudovia::{PseudoVia, RegisterExp};
use super::via::Via;
use crate::bus::access::{Bus68020, BusAccess, BusResponse, bus_access_byte, bus_access_long};
use crate::bus::native_clock::{DeviceCycle, NativeClock};
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::macii::bus::RAM_DIRTY_PAGESIZE;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::via_common::{LineState, ViaLine};
use crate::renderer::{AudioProvider, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent};

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use serde::{Deserialize, Serialize};

/// Wait states of a bus cycle
const BUS_WAITS: Ticks = 1;

/// Frequency of the bus of the real hardware, without overclocking: C16M,
/// which the CPU runs at
const BUS_FREQUENCY: Ticks = 15_667_200;

/// Period of the 60.15 Hz interrupt, in VIA clock cycles
const TICK60_PERIOD: Ticks = 13024;

/// Size of the RAM soldered on the logic board, the rest is in SIMMs
const RAM_SOLDERED: usize = 2 * 1024 * 1024;

/// Translates an address to an index in the RAM, which holds the SIMMs
/// followed by the soldered RAM.
///
/// The SIMM bank starts at 0 and the soldered bank follows it, both with
/// the size configured in the expansion register of the V8.
fn ram_index(exp: RegisterExp, ram_size: usize, addr: Address) -> Option<usize> {
    const MB: usize = 1024 * 1024;

    let addr = addr as usize;
    let simm_size = ram_size - RAM_SOLDERED;
    let simm_bank = [0, 2 * MB, 4 * MB, 8 * MB][exp.ram_simm() as usize];
    if addr < simm_bank {
        return (addr < simm_size).then_some(addr);
    }

    let soldered_bank = if exp.ram_soldered_2mb() {
        2 * MB
    } else {
        4 * MB
    };
    // The ROM tells 2 MB of soldered RAM from 4 MB by writing in the lower half
    // of the bank configured as 4 MB, so 2 MB are taken to be in the upper half.
    // This has not been verified on hardware.
    let hole = soldered_bank - RAM_SOLDERED;
    let offset = addr - simm_bank;
    (hole..soldered_bank)
        .contains(&offset)
        .then(|| simm_size + offset - hole)
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct MacLCBus<TRenderer: Renderer> {
    /// The currently emulated Macintosh model
    model: MacModel,

    rom_mask: usize,
    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    #[serde(skip)]
    renderer: Option<TRenderer>,

    pub(crate) via: Via,
    tick60_clock: Ticks,
    pub(crate) scc: Scc,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    pseudovia: PseudoVia,
    rtc: Rtc,

    overlay: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Counts clock ticks at native frequency, regardless of overclocking
    native_clock: NativeClock,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Mouse mode
    mouse_mode: MouseMode,
}

impl<TRenderer> MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Sense code of the 13" high resolution RGB monitor
    const MONITOR_SENSE: u8 = 0b110;

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
            model,

            rom_mask: rom.len() - 1,
            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            renderer: Some(renderer),
            via: Via::new(model),
            tick60_clock: 0,
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), BUS_FREQUENCY),
            scsi: ScsiController::new(),
            pseudovia: PseudoVia::new(Self::MONITOR_SENSE),
            rtc: Rtc::default(),

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(BUS_FREQUENCY, 20),
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

    /// The V8 only decodes A31 and A23-A0. A31 selects the expansion slot,
    /// which is not implemented, and is ignored in the 24-bit address map.
    fn translate(&self, addr: Address) -> Option<Address> {
        if self.pseudovia.b().fc3() && addr & (1 << 31) != 0 {
            None
        } else {
            Some(addr & 0x00FF_FFFF)
        }
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
                if let Some(idx) = ram_index(self.pseudovia.exp(), self.ram.len(), addr) {
                    self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                    self.ram[idx] = val;
                }
                Some(())
            }
            // ROM
            0xA0_0000..=0xAF_FFFF => Some(()),
            // VIA 1
            0xF0_0000..=0xF0_1FFF => self.via.write(addr, val),
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
            0xF2_6000..=0xF2_7FFF => self.pseudovia.write(addr, val),
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
            0x00_0000..=0x9F_FFFF => Some(
                ram_index(self.pseudovia.exp(), self.ram.len(), addr)
                    .map_or(Self::OPENBUS, |idx| self.ram[idx]),
            ),
            // ROM
            0xA0_0000..=0xAF_FFFF => self.read_rom(addr),
            // VIA 1
            0xF0_0000..=0xF0_1FFF => self.via.read(addr),
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
            0xF2_6000..=0xF2_7FFF => self.pseudovia.read(addr),
            // Video RAM
            0xF4_0000..=0xFB_FFFF => Some(Self::dev_read(addr, None)),
            _ => None,
        }
    }

    fn read_byte(&mut self, addr: Address) -> Option<Byte> {
        let val = match self.translate(addr) {
            Some(addr) if self.overlay => self.read_overlay(addr),
            Some(addr) => self.read_normal(addr),
            None => None,
        };

        if val.is_none() {
            warn!("Bus error (read) at unmapped address: {:08X}", addr);
        }
        val
    }

    fn write_byte(&mut self, addr: Address, val: Byte) -> Option<()> {
        let written = match self.translate(addr) {
            Some(addr) if self.overlay => self.write_overlay(addr, val),
            Some(addr) => self.write_normal(addr, val),
            None => None,
        };

        // Sync values that live in multiple places
        self.swim.sel = self.via.pa().headsel();

        if written.is_none() {
            warn!(
                "Bus error (write) at unmapped address: {:08X} {:02X}",
                addr, val
            );
        }
        written
    }

    fn read_bytes(&mut self, addr: Address, len: u32) -> Option<u32> {
        (addr..addr + len).try_fold(0, |v, a| Some((v << 8) | u32::from(self.read_byte(a)?)))
    }

    fn write_bytes(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        for a in addr..addr + len {
            self.write_byte(a, (val >> (8 * (addr + len - 1 - a))) as Byte)?;
        }
        Some(())
    }

    /// VIA 1 runs at C16M / 20 and its accesses are synchronous with its
    /// clock, as on the Mac II: one starts on a VIA tick and lasts a VIA
    /// clock cycle.
    fn via_cycle(&mut self, acc: BusAccess<Bus68020>) -> BusResponse {
        match self.native_clock.via_sync(&acc) {
            DeviceCycle::Stall(n) => BusResponse::Stall(n),
            DeviceCycle::Access(w) => {
                bus_access_byte(acc, w, self, Self::read_bytes, Self::write_bytes)
            }
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
    type Cpu = Bus68020;

    fn access(&mut self, acc: BusAccess<Bus68020>) -> Result<BusResponse> {
        if let Some(0xF0_0000..=0xF0_1FFF) = self.translate(acc.addr) {
            return Ok(self.via_cycle(acc));
        }
        Ok(bus_access_long(
            acc,
            BUS_WAITS,
            self,
            Self::read_bytes,
            Self::write_bytes,
        ))
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
        self.via.reset();
        self.pseudovia.reset();
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
            bus_frequency: self.bus_frequency(),
        };

        // The devices run at the native frequency, whatever the CPU is
        // overclocked to
        let mut native_ticks = 0;
        let mut cpu = ticks;
        while cpu > 0 {
            let step = self.native_clock.advance(cpu, None);
            if step.native == 0 {
                break;
            }
            cpu -= step.cpu_used;
            native_ticks += step.native;

            // The VIA clock is C16M / 20 (783.36 kHz)
            if step.via_tick {
                self.via.tick(1)?;

                // 60.15 Hz interrupt, generated by the V8
                self.tick60_clock += 1;
                if self.tick60_clock >= TICK60_PERIOD {
                    self.tick60_clock = 0;
                    self.via.set_line(ViaLine::CA1, LineState::Pulse);
                }
            }
        }

        self.scsi.tick(native_ticks, ctx)?;
        self.pseudovia
            .set_scsi(self.scsi.get_irq(), self.scsi.get_drq());
        self.swim.tick(native_ticks, ())?;

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
        if self.pseudovia.irq() {
            return Some(2);
        }
        if self.via.irq() {
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
        let addr = self.translate(addr)?;
        match addr {
            0x00_0000..=0x9F_FFFF if self.overlay => self.read_rom(addr),
            0x00_0000..=0xAF_FFFF => self.read_normal(addr),
            _ => None,
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        let addr = self.translate(addr)?;
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
            dbgprop_nest!("V8 registers", self.pseudovia),
            dbgprop_nest!("VIA 1", self.via),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    fn index(exp: u8, ram_mb: usize, addr: Address) -> Option<usize> {
        ram_index(RegisterExp(exp), ram_mb * MB, addr)
    }

    #[test]
    fn ram_soldered_only() {
        // No SIMM bank, soldered bank of 2 MB
        assert_eq!(index(0x20, 2, 0x00_0000), Some(0));
        assert_eq!(index(0x20, 2, 0x1F_FFFF), Some(2 * MB - 1));
        assert_eq!(index(0x20, 2, 0x20_0000), None);
        assert_eq!(index(0x20, 2, 0x80_0000), None);
    }

    #[test]
    fn ram_soldered_size_probe() {
        // Soldered bank of 4 MB: the lower half does not respond
        assert_eq!(index(0x00, 2, 0x1F_FFFE), None);
        assert_eq!(index(0x00, 2, 0x20_0000), Some(0));
        assert_eq!(index(0xC0, 10, 0x80_0000), None);
        // Soldered bank of 2 MB after a SIMM bank of 8 MB
        assert_eq!(index(0xE0, 10, 0x80_0000), Some(8 * MB));
        assert_eq!(index(0xE0, 10, 0x9F_FFFF), Some(10 * MB - 1));
    }

    #[test]
    fn ram_simm() {
        // SIMM bank of 8 MB with 2 MB fitted
        assert_eq!(index(0xE0, 4, 0x00_0000), Some(0));
        assert_eq!(index(0xE0, 4, 0x1F_FFFF), Some(2 * MB - 1));
        assert_eq!(index(0xE0, 4, 0x20_0000), None);
        assert_eq!(index(0xE0, 4, 0x80_0000), Some(2 * MB));
        // SIMM bank of 2 MB, the soldered bank follows it
        assert_eq!(index(0x60, 4, 0x1F_FFFF), Some(2 * MB - 1));
        assert_eq!(index(0x60, 4, 0x20_0000), Some(2 * MB));
        assert_eq!(index(0x60, 4, 0x3F_FFFF), Some(4 * MB - 1));
        assert_eq!(index(0x60, 4, 0x40_0000), None);
    }
}
