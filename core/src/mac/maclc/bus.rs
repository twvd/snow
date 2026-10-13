use std::thread;
use std::time::{Duration, Instant};

use super::pseudovia::PseudoVia;
use super::via::Via;
use super::video::Video;
use super::{sonora, v8};
use crate::bus::access::{Bus68020, BusAccess, BusResponse, bus_access_byte, bus_access_long};
use crate::bus::native_clock::{DeviceCycle, NativeClock};
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::egret::Egret;
use crate::mac::macii::bus::RAM_DIRTY_PAGESIZE;
use crate::mac::rtc::Rtc;
use crate::mac::scc::Scc;
use crate::mac::scsi::controller::ScsiController;
use crate::mac::sound::v8_sonora::{V8Sound, V8Variant};
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

/// Wait states of a bus cycle
const BUS_WAITS: Ticks = 1;

/// Frequency of the bus of the LC and LC II, without overclocking: C16M,
/// which the CPU runs at
const V8_BUS_FREQUENCY: Ticks = 15_667_200;

/// Frequency of the bus of the LC III, without overclocking
const SONORA_BUS_FREQUENCY: Ticks = 25_000_000;

/// Frequency of the main clock (C16M)
pub(super) const C16M_FREQUENCY: Ticks = 15_667_200;

/// Period of the 60.15 Hz interrupt, in VIA clock cycles
const TICK60_PERIOD: Ticks = 13024;

/// Gate array of the logic board
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum GateArray {
    /// Macintosh LC and LC II
    V8,
    /// Macintosh LC III
    Sonora,
}

impl GateArray {
    /// Frequency of the bus of the real hardware
    const fn bus_frequency(self) -> Ticks {
        match self {
            Self::V8 => V8_BUS_FREQUENCY,
            Self::Sonora => SONORA_BUS_FREQUENCY,
        }
    }
}

/// What an address selects
pub(super) enum Region {
    /// RAM area, with the address in it
    Ram(Address),
    Rom,
    /// I/O device, with the offset in the I/O area
    Io(Address),
    /// Video RAM, with the offset in its window
    Vram(usize),
    /// CPU ID register of the Sonora, with the number of the byte
    CpuId(usize),
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct MacLCBus<TRenderer: Renderer> {
    /// The currently emulated Macintosh model
    model: MacModel,

    gate_array: GateArray,

    rom_mask: usize,
    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// Size of the RAM soldered on the logic board, the rest is in SIMMs
    ram_soldered: usize,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via: Via,
    egret: Egret,
    tick60_clock: Ticks,
    pub(crate) scc: Scc,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    pseudovia: PseudoVia,
    video: Video<TRenderer>,
    sound: V8Sound,
    sound_clock: Ticks,

    overlay: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Counts clock ticks at native frequency, regardless of overclocking
    native_clock: NativeClock,

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
        let gate_array = if model == MacModel::LCIII {
            GateArray::Sonora
        } else {
            GateArray::V8
        };
        let ram_soldered = if model == MacModel::LC {
            2 * MB
        } else {
            4 * MB
        };
        // 10 MB on the LC II are 8 MB of SIMMs, with which only 2 MB of the
        // 4 MB of soldered RAM can be used
        let ram_simm = match ram_size - ram_soldered {
            n if model == MacModel::LCII && n == 6 * MB => 8 * MB,
            n => n,
        };
        let ram_size = ram_simm + ram_soldered;
        let monitor = monitor
            .filter(|m| v8::MONITORS.contains(m))
            .unwrap_or_default();
        let vram_size = vram_size
            .filter(|s| v8::VRAM_SIZES.contains(s))
            .unwrap_or(v8::VRAM_SIZE_DEFAULT);
        let video = Video::new(renderer, monitor, vram_size);
        let mut egret = Egret::default();
        egret.adb_add_device(AdbMouse::new());
        egret.adb_add_device(AdbKeyboard::new());

        Self {
            model,
            gate_array,

            rom_mask: rom.len() - 1,
            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_soldered,
            ram_dirty: BitSet::from_iter(0..(ram_size / RAM_DIRTY_PAGESIZE)),
            via: Via::new(model),
            egret,
            tick60_clock: 0,
            scc: Scc::new(),
            swim: Swim::new(
                model.fdd_drives(),
                model.fdd_hd(),
                gate_array.bus_frequency(),
            ),
            scsi: ScsiController::new(),
            pseudovia: PseudoVia::new(
                video.sense(),
                match gate_array {
                    GateArray::V8 => v8::PSEUDOVIA_MASK,
                    GateArray::Sonora => sonora::PSEUDOVIA_MASK,
                },
            ),
            video,
            sound: V8Sound::new(match gate_array {
                GateArray::V8 => V8Variant::V8,
                GateArray::Sonora => V8Variant::Sonora,
            }),
            sound_clock: 0,

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(gate_array.bus_frequency()),
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

    /// Finds what an address selects
    fn decode(&self, addr: Address) -> Option<Region> {
        match self.gate_array {
            GateArray::V8 => v8::decode(addr, self.pseudovia.b().fc3()),
            GateArray::Sonora => sonora::decode(addr),
        }
    }

    /// Translates an address in the RAM area to an index in the RAM, if
    /// there is RAM at the address
    fn ram_index(&self, addr: Address) -> Option<usize> {
        let exp = self.pseudovia.exp();
        let simm_size = self.ram.len() - self.ram_soldered;
        match self.gate_array {
            GateArray::V8 => v8::ram_index(exp, simm_size, self.ram_soldered, addr),
            GateArray::Sonora => sonora::ram_index(exp.0, simm_size, self.ram_soldered, addr),
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

    fn read_ram_byte(&self, addr: Address) -> Byte {
        self.ram_index(addr)
            .map_or(Self::OPENBUS, |idx| self.ram[idx])
    }

    fn write_ram_byte(&mut self, addr: Address, val: Byte) {
        if let Some(idx) = self.ram_index(addr) {
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
            self.ram[idx] = val;
        }
    }

    /// Translates an offset in the window of the video RAM to an index in
    /// the video RAM, if there is video RAM at the offset
    fn vram_index(&self, offset: usize) -> Option<usize> {
        match self.gate_array {
            GateArray::V8 => Some(v8::vram_index(self.video.vram_size(), offset)),
            // The video of the Sonora is not implemented
            GateArray::Sonora => None,
        }
    }

    fn read_vram(&self, offset: usize) -> Byte {
        self.vram_index(offset)
            .map_or(Self::OPENBUS, |idx| self.video.read_vram(idx))
    }

    fn write_vram(&mut self, offset: usize, val: Byte) {
        if let Some(idx) = self.vram_index(offset) {
            self.video.write_vram(idx, val);
        }
    }

    /// Address of the I/O area, for the devices and the log
    fn io_base(&self) -> Address {
        match self.gate_array {
            GateArray::V8 => v8::IO_BASE,
            GateArray::Sonora => sonora::IO_BASE,
        }
    }

    /// Writes to an I/O device. `offset` is the offset in the I/O area.
    fn write_io(&mut self, offset: Address, val: Byte) -> Option<()> {
        let addr = self.io_base() | offset;
        match offset {
            // VIA 1
            0x0_0000..=0x0_1FFF => self.via.write(addr, val),
            // SCC
            0x0_4000..=0x0_5FFF => Self::dev_write(addr, val, self.scc.write(addr >> 1, val)),
            // SCSI
            0x0_6000..=0x0_7FFF => Some(self.scsi.write_dma(val)),
            0x1_0000..=0x1_1FFF => Self::dev_write(addr, val, self.scsi.write(addr, val)),
            0x1_2000..=0x1_3FFF => Some(self.scsi.write_dma(val)),
            // Sound
            0x1_4000..=0x1_5FFF => Self::dev_write(addr, val, self.sound.write(addr & 0xFFF, val)),
            // SWIM
            0x1_6000..=0x1_7FFF => Self::dev_write(addr, val, self.swim.write(addr, val)),
            // RAMDAC
            0x2_4000..=0x2_5FFF => Some(self.video.write_ramdac(addr, val)),
            // V8 registers
            0x2_6000..=0x2_7FFF => self.pseudovia.write(addr, val),
            _ => None,
        }
    }

    /// Reads from an I/O device. `offset` is the offset in the I/O area.
    fn read_io(&mut self, offset: Address) -> Option<Byte> {
        let addr = self.io_base() | offset;
        match offset {
            // VIA 1
            0x0_0000..=0x0_1FFF => self.via.read(addr),
            // SCC
            0x0_4000..=0x0_5FFF => Some(Self::dev_read(addr, self.scc.read(addr >> 1))),
            // SCSI
            0x0_6000..=0x0_7FFF => Some(self.scsi.read_dma()),
            0x1_0000..=0x1_1FFF => Some(Self::dev_read(addr, self.scsi.read(addr))),
            0x1_2000..=0x1_3FFF => Some(self.scsi.read_dma()),
            // Sound
            0x1_4000..=0x1_5FFF => Some(Self::dev_read(addr, self.sound.read(addr & 0xFFF))),
            // SWIM
            0x1_6000..=0x1_7FFF => Some(Self::dev_read(addr, self.swim.read(addr))),
            // RAMDAC
            0x2_4000..=0x2_5FFF => Some(self.video.read_ramdac(addr)),
            // V8 registers
            0x2_6000..=0x2_7FFF => self.pseudovia.read(addr),
            _ => None,
        }
    }

    fn read_byte(&mut self, addr: Address) -> Option<Byte> {
        let val = match self.decode(addr) {
            // ROM (overlay)
            Some(Region::Ram(_)) if self.overlay => self.read_rom(addr),
            Some(Region::Ram(addr)) => Some(self.read_ram_byte(addr)),
            Some(Region::Rom) => {
                self.overlay = false;
                self.read_rom(addr)
            }
            Some(Region::Io(offset)) => self.read_io(offset),
            Some(Region::Vram(offset)) => Some(self.read_vram(offset)),
            Some(Region::CpuId(n)) => Some(sonora::CPU_ID_LCIII.to_be_bytes()[n]),
            None => None,
        };

        if val.is_none() {
            warn!("Bus error (read) at unmapped address: {:08X}", addr);
        }
        val
    }

    fn write_byte(&mut self, addr: Address, val: Byte) -> Option<()> {
        let written = match self.decode(addr) {
            Some(Region::Ram(addr)) => Some(self.write_ram_byte(addr, val)),
            Some(Region::Rom) => Some(()),
            Some(Region::Io(offset)) => self.write_io(offset, val),
            Some(Region::Vram(offset)) => Some(self.write_vram(offset, val)),
            Some(Region::CpuId(_)) => Some(()),
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
            self.write_ram_byte(addr, b);
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

        // Report updated mouse coordinates to OS. The current (RawMouse) and
        // new (MTemp) positions are the same, as the LC III ROM takes any
        // difference between them for a relative movement.
        self.write_ram(Self::ADDR_MTEMP_X, &x.to_be_bytes());
        self.write_ram(Self::ADDR_MTEMP_Y, &y.to_be_bytes());
        self.write_ram(Self::ADDR_RAWMOUSE_X, &x.to_be_bytes());
        self.write_ram(Self::ADDR_RAWMOUSE_Y, &y.to_be_bytes());
        self.write_ram(Self::ADDR_CRSRNEW, &[1]);
    }

    /// Configures emulator speed
    pub fn set_speed(&mut self, speed: EmulatorSpeed) {
        info!("Emulation speed: {:?}", speed);
        self.speed = speed;
    }

    /// Frequency of the bus of the real hardware, without overclocking
    #[inline(always)]
    pub const fn bus_frequency(&self) -> Ticks {
        self.gate_array.bus_frequency()
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
    type Cpu = Bus68020;

    fn access(&mut self, acc: BusAccess<Bus68020>) -> Result<BusResponse> {
        if let Some(Region::Io(0x0_0000..=0x0_1FFF)) = self.decode(acc.addr) {
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

            // The VIA clock is C16M / 20 (783.36 kHz), whatever the frequency
            // of the bus is
            if step.via_tick {
                self.via.tick(1)?;

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
        }

        // Host frame pacing
        self.vblank_clock += native_ticks;
        while self.vblank_clock >= self.bus_frequency() / 60 {
            self.vblank_clock -= self.bus_frequency() / 60;

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

        let monitor = self.video.monitor();
        let frame_period = v8::frame_period(monitor);
        if self
            .video
            .tick(native_ticks, self.bus_frequency(), frame_period)
        {
            match self.gate_array {
                GateArray::V8 => {
                    let mode = v8::video_mode(monitor, self.pseudovia.exp(), self.pseudovia.monp());
                    self.video.render(&mode)?;
                }
                // The video of the Sonora is not implemented
                GateArray::Sonora => self.video.blank()?,
            }
            self.pseudovia.set_vblank();
        }

        // Sound. Queuing the samples paces the emulation in the Accurate mode.
        self.sound_clock += native_ticks * C16M_FREQUENCY;
        while self.sound_clock >= self.sound.sample_period() * self.bus_frequency() {
            self.sound_clock -= self.sound.sample_period() * self.bus_frequency();

            self.sound.tick(self.speed == EmulatorSpeed::Accurate)?;
        }
        self.pseudovia.set_asc_irq(self.sound.irq());

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
        match self.decode(addr)? {
            Region::Ram(_) if self.overlay => self.read_rom(addr),
            Region::Ram(addr) => Some(self.read_ram_byte(addr)),
            Region::Rom => self.read_rom(addr),
            _ => None,
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match self.decode(addr)? {
            Region::Ram(addr) => Some(self.write_ram_byte(addr, val)),
            Region::Rom => Some(()),
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
            dbgprop_nest!("Gate array registers", self.pseudovia),
            dbgprop_nest!("Video", self.video),
            dbgprop_nest!("VIA 1", self.via),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    /// Seconds counted by the Egret, which is clocked like the VIA, while the
    /// bus runs for a second less `less` cycles
    fn clock_seconds(model: MacModel, overclock: Option<u64>, less: Ticks) -> u32 {
        let mut bus = MacLCBus::new(
            model,
            &vec![0; 512 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MouseMode::Disabled,
            None,
            None,
            None,
        );
        bus.set_speed(EmulatorSpeed::Uncapped);
        bus.set_overclock(overclock);

        let start = bus.rtc_mut().seconds();
        let cycles = overclock.unwrap_or_else(|| bus.bus_frequency()) - less;
        for _ in 0..cycles / 20 {
            bus.tick(20, ()).unwrap();
        }
        bus.rtc_mut().seconds() - start
    }

    #[test]
    fn via_clock_follows_bus_frequency() {
        assert_eq!(clock_seconds(MacModel::LC, None, 0), 1);
        assert_eq!(clock_seconds(MacModel::LC, None, 20), 0);
        // 25 MHz
        assert_eq!(clock_seconds(MacModel::LCIII, None, 0), 1);
        assert_eq!(clock_seconds(MacModel::LCIII, None, 20), 0);
        // Overclocked
        assert_eq!(clock_seconds(MacModel::LC, Some(32_000_000), 0), 1);
        assert_eq!(clock_seconds(MacModel::LC, Some(32_000_000), 20), 0);
    }
}
