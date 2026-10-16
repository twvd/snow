use std::thread;
use std::time::{Duration, Instant};

use super::pseudovia::{PseudoVia, RegisterExp};
use super::sound::Sound;
use super::via::Via;
use super::video::Video;
use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::egret::Egret;
use crate::mac::macii::bus::{DEFAULT_BUS_SPEED, RAM_DIRTY_PAGESIZE};
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
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
use serde::{Deserialize, Serialize};

/// Frequency of the main clock (C16M)
pub(super) const C16M_FREQUENCY: Ticks = 15_667_200;

/// Period of the 60.15 Hz interrupt, in VIA clock cycles
const TICK60_PERIOD: Ticks = 13024;

/// Start of the window of the video RAM
const VRAM_BASE: usize = 0xF4_0000;

/// Translates an address to an index in the RAM, which holds the SIMMs
/// followed by the RAM soldered on the logic board.
///
/// The SIMM bank starts at 0 and the soldered bank follows it, both with
/// the size configured in the expansion register of the V8.
fn ram_index(
    exp: RegisterExp,
    simm_size: usize,
    soldered_size: usize,
    addr: Address,
) -> Option<usize> {
    const MB: usize = 1024 * 1024;

    let addr = addr as usize;
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
    let hole = soldered_bank.saturating_sub(soldered_size);
    let offset = addr - simm_bank;
    (hole..soldered_bank)
        .contains(&offset)
        .then(|| simm_size + offset - hole)
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct MacLCBus<TRenderer: Renderer> {
    cycles: Ticks,

    /// The currently emulated Macintosh model
    model: MacModel,

    rom_mask: usize,
    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// Size of the RAM soldered on the logic board, the rest is in SIMMs
    ram_soldered: usize,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via: Via,
    egret: Egret,
    via_clock: Ticks,
    tick60_clock: Ticks,
    pub(crate) scc: Scc,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    pseudovia: PseudoVia,
    video: Video<TRenderer>,
    sound: Sound,
    sound_clock: Ticks,

    overlay: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    bus_frequency: Ticks,

    /// Time at which the last frame was due (for pacing to the host)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,
    vblank_clock: Ticks,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Mouse mode
    mouse_mode: MouseMode,

    /// The system has initialized the mouse position (absolute mouse mode)
    mouse_ready: bool,
}

impl<TRenderer> MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Value to return on open bus
    const OPENBUS: u8 = 0xFF;

    /// MTemp address, Y coordinate (16 bit, signed)
    const ADDR_MTEMP_Y: Address = 0x0828;
    /// MTemp address, X coordinate (16 bit, signed)
    const ADDR_MTEMP_X: Address = 0x082A;
    /// RawMouse address, Y coordinate (16 bit, signed)
    const ADDR_RAWMOUSE_Y: Address = 0x082C;
    /// RawMouse address, X coordinate (16 bit, signed)
    const ADDR_RAWMOUSE_X: Address = 0x082E;
    /// CrsrNew address
    const ADDR_CRSRNEW: Address = 0x08CE;

    pub fn new(
        model: MacModel,
        rom: &[u8],
        renderer: TRenderer,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
        monitor: Option<MacMonitor>,
        vram_size: Option<usize>,
    ) -> Self {
        const MB: usize = 1024 * 1024;

        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
        let ram_soldered = if model == MacModel::LCII {
            4 * MB
        } else {
            2 * MB
        };
        // 10 MB on the LC II are 8 MB of SIMMs, with which only 2 MB of the
        // 4 MB of soldered RAM can be used
        let ram_simm = match ram_size - ram_soldered {
            n if n == 6 * MB => 8 * MB,
            n => n,
        };
        let ram_size = ram_simm + ram_soldered;
        let video = Video::new(renderer, monitor, vram_size);
        let mut egret = Egret::default();
        egret.adb_add_device(AdbMouse::new());
        egret.adb_add_device(AdbKeyboard::new());

        Self {
            cycles: 0,
            model,

            rom_mask: rom.len() - 1,
            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_soldered,
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            via: Via::new(model),
            egret,
            via_clock: 0,
            tick60_clock: 0,
            scc: Scc::new(),
            swim: Swim::new(model.fdd_drives(), model.fdd_hd(), DEFAULT_BUS_SPEED),
            scsi: ScsiController::new(),
            pseudovia: PseudoVia::new(video.sense()),
            video,
            sound: Sound::default(),
            sound_clock: 0,

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            bus_frequency: DEFAULT_BUS_SPEED,
            vblank_time: Instant::now(),
            vblank_clock: 0,
            progkey_pressed: LatchingEvent::default(),
            mouse_mode,
            mouse_ready: false,
        }
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        self.video.renderer = Some(renderer);
        self.sound.after_deserialize();

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.egret.rtc().effective_speed()
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        // The sample rate is 22.257 kHz, rounded down to a commonly used value
        let sink = provider
            .create_stream(22050, AUDIO_CHANNELS as u8, AUDIO_BUFFER_SAMPLES as u16)
            .unwrap_or_else(|_| null_audio_sink());
        self.sound.set_sink(sink);

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

    fn ram_index(&self, addr: Address) -> Option<usize> {
        ram_index(
            self.pseudovia.exp(),
            self.ram.len() - self.ram_soldered,
            self.ram_soldered,
            addr,
        )
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
                if let Some(idx) = self.ram_index(addr) {
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
            // Sound
            0xF1_4000..=0xF1_5FFF => {
                Self::dev_write(addr, val, self.sound.write(addr & 0xFFF, val))
            }
            // SWIM
            0xF1_6000..=0xF1_7FFF => Self::dev_write(addr, val, self.swim.write(addr, val)),
            // RAMDAC
            0xF2_4000..=0xF2_5FFF => Some(self.video.write_ramdac(addr, val)),
            // V8 registers
            0xF2_6000..=0xF2_7FFF => self.pseudovia.write(addr, val),
            // Video RAM
            0xF4_0000..=0xFB_FFFF => Some(self.video.write_vram(addr as usize - VRAM_BASE, val)),
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
                self.ram_index(addr)
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
            // Sound
            0xF1_4000..=0xF1_5FFF => Some(Self::dev_read(addr, self.sound.read(addr & 0xFFF))),
            // SWIM
            0xF1_6000..=0xF1_7FFF => Some(Self::dev_read(addr, self.swim.read(addr))),
            // RAMDAC
            0xF2_4000..=0xF2_5FFF => Some(self.video.read_ramdac(addr)),
            // V8 registers
            0xF2_6000..=0xF2_7FFF => self.pseudovia.read(addr),
            // Video RAM
            0xF4_0000..=0xFB_FFFF => Some(self.video.read_vram(addr as usize - VRAM_BASE)),
            _ => None,
        }
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        if self.mouse_mode == MouseMode::Disabled {
            return;
        }

        if button.is_some() {
            self.egret.adb_event(&AdbEvent::Mouse(MouseEvent {
                button,
                rel_movement: None,
            }));
        }

        if self.mouse_mode == MouseMode::RelativeHw && (relx != 0 || rely != 0) {
            self.egret.adb_event(&AdbEvent::Mouse(MouseEvent {
                button: None,
                rel_movement: Some((relx.into(), rely.into())),
            }));
        }
    }

    /// Reads a 16-bit value from RAM at an address of the CPU
    fn read_ram16(&self, addr: Address) -> u16 {
        let byte = |addr| self.ram_index(addr).map_or(0, |idx| self.ram[idx]);
        u16::from_be_bytes([byte(addr), byte(addr + 1)])
    }

    /// Writes bytes to RAM at an address of the CPU
    fn write_ram(&mut self, addr: Address, val: &[u8]) {
        for (addr, &b) in (addr..).zip(val) {
            self.write_normal(addr, b);
        }
    }

    /// Updates the mouse position (absolute coordinates)
    pub fn mouse_update_abs(&mut self, x: u16, y: u16) {
        if self.mouse_mode != MouseMode::Absolute {
            return;
        }

        let old_x = self.read_ram16(Self::ADDR_RAWMOUSE_X);
        let old_y = self.read_ram16(Self::ADDR_RAWMOUSE_Y);

        if !self.mouse_ready && (old_x != 15 || old_y != 15) {
            // Wait until the boot process has initialized the mouse position so we don't
            // interfere with the memory test.
            return;
        }
        self.mouse_ready = true;

        // Report updated mouse coordinates to OS. It needs to see a small
        // difference between the current (RawMouse) and new (MTemp) position,
        // otherwise the change is ignored.
        self.write_ram(Self::ADDR_MTEMP_X, &x.to_be_bytes());
        self.write_ram(Self::ADDR_MTEMP_Y, &y.to_be_bytes());
        self.write_ram(Self::ADDR_RAWMOUSE_X, &x.wrapping_sub(1).to_be_bytes());
        self.write_ram(Self::ADDR_RAWMOUSE_Y, &y.wrapping_add(1).to_be_bytes());
        self.write_ram(Self::ADDR_CRSRNEW, &[1]);
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

    /// Programmer's key pressed
    pub fn progkey(&mut self) {
        self.progkey_pressed.set();
    }

    pub fn video_blank(&mut self) -> Result<()> {
        self.video.blank()
    }

    /// Dispatches a key event to the keyboard
    pub fn keyboard_event(&mut self, ke: KeyEvent) {
        self.egret.adb_event(&AdbEvent::Key(ke));
    }

    /// Releases all pressed inputs
    pub fn input_release_all(&mut self) {
        self.egret.adb_event(&AdbEvent::ReleaseAll);
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        self.egret.rtc_mut()
    }
}

impl<TRenderer> Bus<Address, Byte> for MacLCBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn read(&mut self, addr: Address) -> BusResult<Byte> {
        let val = match self.translate(addr) {
            Some(addr) if self.overlay => self.read_overlay(addr),
            Some(addr) => self.read_normal(addr),
            None => None,
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
        self.via.reset();
        self.pseudovia.reset();
        self.sound.reset();
        self.overlay = true;
        self.mouse_ready = false;
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

        // The VIA clock is C16M / 20 (783.36 kHz). As the bus runs at a nominal
        // 16 MHz, that is slightly less than once every 20 bus cycles.
        self.via_clock += ticks * C16M_FREQUENCY;
        while self.via_clock >= 20 * DEFAULT_BUS_SPEED {
            // TODO VIA wait states
            self.via_clock -= 20 * DEFAULT_BUS_SPEED;

            self.via.tick(1, ctx)?;

            let pb = self.via.pb();
            let via = &mut self.via;
            self.egret
                .tick(1, pb.sys_ses(), pb.via_full(), |b| via.shift_sr(b));
            self.via.b_in.set_xcvr_ses(!self.egret.xcvr_ses());

            // 60.15 Hz interrupt, generated by the V8
            self.tick60_clock += 1;
            if self.tick60_clock >= TICK60_PERIOD {
                self.tick60_clock = 0;
                self.via.set_line(ViaLine::CA1, LineState::Pulse);
            }
        }

        // Host frame pacing
        self.vblank_clock += ticks;
        while self.vblank_clock >= self.bus_frequency / 60 {
            self.vblank_clock -= self.bus_frequency / 60;

            let max_speed = match self.speed {
                // Sync to 60 fps video
                EmulatorSpeed::Video => Some(1.0),
                // Cap to the requested speedup multiplier
                EmulatorSpeed::FastForward(max_speed) => Some(max_speed),
                _ => None,
            };
            if let Some(max_speed) = max_speed {
                // Sleep until the time at which this frame is due, counted from
                // the time the last one was due so that the error of the sleep
                // does not add up.
                let due = self.vblank_time + Duration::from_secs_f64(1.0 / (60.0 * max_speed));
                let now = Instant::now();
                if now < due {
                    thread::sleep(due - now);
                    self.vblank_time = due;
                } else {
                    self.vblank_time = now;
                }
            }
        }

        if self.video.tick(ticks, self.bus_frequency) {
            let exp = self.pseudovia.exp();
            let row_bytes = if exp.vram_256k() { 512 } else { 1024 };
            self.video
                .render(self.pseudovia.monp().depth(), row_bytes)?;
            self.pseudovia.set_vblank();
        }

        // Sound. Queuing the samples paces the emulation in the Accurate mode.
        self.sound_clock += ticks;
        while self.sound_clock >= self.bus_frequency / self.sound.sample_rate() {
            self.sound_clock -= self.bus_frequency / self.sound.sample_rate();

            self.sound.tick(self.speed == EmulatorSpeed::Accurate)?;
        }
        self.pseudovia.set_asc_irq(self.sound.irq());

        self.scsi.tick(ticks, ctx)?;
        self.pseudovia
            .set_scsi(self.scsi.get_irq(), self.scsi.get_drq());
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
            dbgprop_nest!("Egret", self.egret),
            dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi),
            dbgprop_nest!("Sound", self.sound),
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("V8 registers", self.pseudovia),
            dbgprop_nest!("Video", self.video),
            dbgprop_nest!("VIA 1", self.via),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    fn index(exp: u8, ram_mb: usize, addr: Address) -> Option<usize> {
        ram_index(RegisterExp(exp), (ram_mb - 2) * MB, 2 * MB, addr)
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

    /// Index with the 4 MB of soldered RAM of the Macintosh LC II
    fn index_lcii(exp: u8, simm_mb: usize, addr: Address) -> Option<usize> {
        ram_index(RegisterExp(exp), simm_mb * MB, 4 * MB, addr)
    }

    #[test]
    fn ram_lcii_soldered_only() {
        // Soldered bank of 4 MB: all of it responds
        assert_eq!(index_lcii(0x00, 0, 0x00_0000), Some(0));
        assert_eq!(index_lcii(0x00, 0, 0x1F_FFFE), Some(2 * MB - 2));
        assert_eq!(index_lcii(0x00, 0, 0x3F_FFFF), Some(4 * MB - 1));
        assert_eq!(index_lcii(0x00, 0, 0x40_0000), None);
    }

    #[test]
    fn ram_lcii_simm() {
        // SIMM bank of 8 MB: the soldered bank follows it at 8 MB, where
        // only its first 2 MB are in the RAM area
        assert_eq!(index_lcii(0xC0, 8, 0x7F_FFFF), Some(8 * MB - 1));
        assert_eq!(index_lcii(0xC0, 8, 0x80_0000), Some(8 * MB));
        assert_eq!(index_lcii(0xC0, 8, 0x9F_FFFF), Some(10 * MB - 1));
        // SIMM bank of 8 MB with 2 MB fitted
        assert_eq!(index_lcii(0xC0, 2, 0x20_0000), None);
        assert_eq!(index_lcii(0xC0, 2, 0x80_0000), Some(2 * MB));
        // SIMM banks of 4 MB and 2 MB
        assert_eq!(index_lcii(0x80, 4, 0x40_0000), Some(4 * MB));
        assert_eq!(index_lcii(0x80, 4, 0x7F_FFFF), Some(8 * MB - 1));
        assert_eq!(index_lcii(0x40, 2, 0x20_0000), Some(2 * MB));
        assert_eq!(index_lcii(0x40, 2, 0x5F_FFFF), Some(6 * MB - 1));
        assert_eq!(index_lcii(0x40, 2, 0x60_0000), None);
    }
}
