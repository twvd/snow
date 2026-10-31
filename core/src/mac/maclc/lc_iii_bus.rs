//! Bus of the Macintosh LC III, around the Sonora gate array

use std::thread;
use std::time::{Duration, Instant};

use super::pds_iie::{IieCardSettings, PdsIIeCard};
use super::pseudovia::PseudoVia;
use super::sonora;
use super::via::Via;
use super::video::{self, Video};
use crate::bus::access::{
    Bus68020, BusAccess, BusResponse, bus_access_byte, bus_access_long, bus_access_word,
};
use crate::bus::native_clock::{C16M_FREQUENCY, DeviceCycle, NativeClock};
use crate::bus::{
    Address, Bus, BusMember, InspectRun, InspectableBus, IrqSource, inspect_read_bytes,
};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::egret::Egret;
use crate::mac::rtc::Rtc;
use crate::mac::scc::{RTXC_FREQUENCY, Scc, SccCh};
use crate::mac::scsi::controller::ScsiController;
use crate::mac::sound::v8_sonora::{V8Sound, V8Variant};
use crate::mac::swim::Swim;
use crate::mac::via_common::{LineState, ViaLine};
use crate::mac::{AbsoluteMouse, MacModel, MacMonitor, WARM_START_SIGNATURE};
use crate::renderer::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, Renderer, null_audio_sink,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

/// Frequency of the bus, without overclocking
const BUS_FREQUENCY: Ticks = 25_000_000;

/// Wait states of an I/O bus cycle, when not measured
pub(super) const IO_WAITS: Ticks = 1;

/// Bytes as a big endian number
fn be_bytes(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b))
}

/// Size of the RAM soldered on the logic board
const RAM_SOLDERED: usize = 4 * 1024 * 1024;

/// Period of the 60.15 Hz interrupt, in VIA clock cycles
const TICK60_PERIOD: Ticks = 13024;

/// Swing of the Apple IIe Card's speaker in the sound output, full scale = 1
const IIE_SPEAKER_GAIN: f32 = 0.5;

/// What an address selects
pub(super) enum Region {
    /// RAM area, with the address in it
    Ram(Address),
    Rom,
    /// I/O device, with the offset in the I/O area
    Io(Address),
    /// Video RAM, with the offset in its window
    Vram(usize),
    /// PDS slot $E card, with the offset in the slot window
    IieCard(Address),
    /// CPU ID register, with the number of the byte
    CpuId(usize),
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct LcIIIBus<TRenderer: Renderer> {
    /// The currently emulated Macintosh model
    model: MacModel,

    /// Video registers of the Sonora
    video_regs: sonora::VideoRegs,

    rom_mask: usize,
    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// Size of the RAM soldered on the logic board, the rest is in SIMMs
    ram_soldered: usize,

    pub(crate) via: Via,
    egret: Egret,
    /// Native time the SCC can be accessed again
    scc_ready: Ticks,
    /// Native time the SWIM can be accessed again
    swim_ready: Ticks,
    /// Address of the second half of a long video RAM read
    vram_next_half: Option<Address>,
    /// Native time the video RAM is done with the last write
    vram_ready: Ticks,
    tick60_clock: Ticks,
    pub(crate) scc: Scc,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,
    pseudovia: PseudoVia,
    video: Video<TRenderer>,
    sound: V8Sound,
    sound_clock: Ticks,

    /// Apple IIe Card in the PDS slot, when installed
    iie_card: Option<PdsIIeCard>,

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

impl<TRenderer> LcIIIBus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Value to return on open bus
    const OPENBUS: u8 = 0xFF;

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: MacModel,
        rom: &[u8],
        renderer: TRenderer,
        mouse_mode: MouseMode,
        ram_size: Option<usize>,
        monitor: Option<MacMonitor>,
        vram_size: Option<usize>,
        iie_card: Option<IieCardSettings>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
        let monitor = monitor
            .filter(|m| video::monitors(model).contains(m))
            .unwrap_or_default();
        let vram_size = vram_size
            .filter(|s| video::vram_sizes(model).contains(s))
            .unwrap_or_else(|| video::vram_size_default(model));
        let video = Video::new(renderer, monitor, vram_size);
        let swim = Swim::new_swim2(model.fdd_drives(), BUS_FREQUENCY);
        let mut egret = Egret::default();
        egret.adb_add_device(AdbMouse::new());
        egret.adb_add_device(AdbKeyboard::new());

        let mut bus = Self {
            model,
            video_regs: sonora::VideoRegs::new(),

            rom_mask: rom.len() - 1,
            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_soldered: RAM_SOLDERED,
            via: Via::new(model),
            egret,
            scc_ready: 0,
            swim_ready: 0,
            vram_next_half: None,
            vram_ready: 0,
            tick60_clock: 0,
            scc: Scc::new(RTXC_FREQUENCY),
            swim,
            scsi: ScsiController::new(),
            pseudovia: PseudoVia::new(video.sense(), sonora::PSEUDOVIA_MASK),
            video,
            sound: V8Sound::new(V8Variant::Sonora),
            sound_clock: 0,

            iie_card: iie_card.map(|settings| PdsIIeCard::with_settings(&settings)),

            overlay: true,
            speed: EmulatorSpeed::Accurate,
            native_clock: NativeClock::new(BUS_FREQUENCY),
            vblank_time: Instant::now(),
            vblank_clock: 0,
            progkey_pressed: LatchingEvent::default(),
            mouse_mode,
            mouse_ready: false,
        };

        bus.disable_memtest();

        bus
    }

    /// The display outputs of this machine (the single built-in video output)
    pub fn display_names(&self) -> Vec<String> {
        vec!["Built-in display".to_string()]
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderers: Vec<TRenderer>) {
        self.video.renderer = renderers.into_iter().next();
        self.sound.after_deserialize();
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    /// Sets the Egret's J17 "security" jumper (installed locks out the
    /// Apple IIe Card's Option Panel configuration panels).
    pub fn set_security_jumper(&mut self, installed: bool) {
        self.egret.set_security_jumper(installed);
    }

    /// The Apple IIe card in the PDS slot, if installed.
    pub fn iie_card_mut(&mut self) -> Option<&mut PdsIIeCard> {
        self.iie_card.as_mut()
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

        self.scsi.bus_mut().set_audio_provider(provider)
    }

    /// Finds what an address selects
    fn decode(&self, addr: Address) -> Option<Region> {
        sonora::decode(addr)
    }

    /// Makes the ROM skip its memory test
    ///
    /// This ROM does not look for the warm start signature at $CFC but in the
    /// long just below the memory table that it builds at the top of the first
    /// RAM bank it finds. That is the SIMM, when there is one. The offset was
    /// measured by running the ROM with every RAM size.
    fn disable_memtest(&mut self) {
        let simm_size = self.ram.len() - self.ram_soldered;
        let index = if simm_size != 0 {
            simm_size - 0x38
        } else {
            self.ram.len() - 0x38
        };
        info!("Skipping memory test");
        self.ram[index..index + 4].copy_from_slice(&WARM_START_SIGNATURE.1.to_be_bytes());
    }

    /// Translates an address in the RAM area to an index in the RAM, if
    /// there is RAM at the address
    fn ram_index(&self, addr: Address) -> Option<usize> {
        let exp = self.pseudovia.exp();
        let simm_size = self.ram.len() - self.ram_soldered;
        sonora::ram_index(exp.0, simm_size, self.ram_soldered, addr)
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
            self.ram[idx] = val;
        }
    }

    /// Translates an offset in the window of the video RAM to an index in
    /// the video RAM, if there is video RAM at the offset
    fn vram_index(&self, offset: usize) -> Option<usize> {
        sonora::vram_index(self.pseudovia.vram(), self.video.vram_size(), offset)
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

    /// Writes to an I/O device. `offset` is the offset in the I/O area.
    fn write_io(&mut self, offset: Address, val: Byte) -> Option<()> {
        let addr = sonora::IO_BASE | offset;
        match offset {
            // VIA 1
            0x0_0000..=0x0_1FFF => {
                self.via.write(addr, val)?;
                // The SWIM SEL line is the VIA's PA5 pin
                self.swim.sel = self.via.pa().headsel();
                Some(())
            }
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
            // Sonora registers
            0x2_6000..=0x2_7FFF => self.pseudovia.write(addr, val),
            // Video registers
            0x2_8000..=0x2_9FFF => Some(self.video_regs.write(addr, val)),
            _ => None,
        }
    }

    /// Reads from an I/O device. `offset` is the offset in the I/O area.
    fn read_io(&mut self, offset: Address) -> Option<Byte> {
        let addr = sonora::IO_BASE | offset;
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
            // Sonora registers
            0x2_6000..=0x2_7FFF => self.pseudovia.read(addr),
            // Video registers
            0x2_8000..=0x2_9FFF => Some(self.video_regs.read(addr, self.video.monitor())),
            _ => None,
        }
    }

    /// One cycle of the Apple IIe Card, with the offset in the slot: without
    /// a card, the slot reads open bus and ignores writes.
    fn iie_read(&mut self, offset: Address, len: u32) -> Option<u32> {
        match &mut self.iie_card {
            Some(card) => Some(card.slot_read(offset, len)),
            None => Some(PdsIIeCard::open_bus(len)),
        }
    }

    fn iie_write(&mut self, offset: Address, len: u32, val: u32) -> Option<()> {
        if let Some(card) = &mut self.iie_card {
            card.slot_write(offset, len, val);
        }
        Some(())
    }

    /// Width in bytes and wait states of the port answering a region.
    ///
    /// The Sonora has a 32-bit RAM and ROM and a 16-bit video RAM. The I/O
    /// devices are on D31-D24, as an 8-bit port.
    fn port(&self, region: &Region, write: bool) -> (u32, Ticks) {
        match region {
            Region::Io(offset) => (1, self.io_waits(*offset, write)),
            Region::Ram(_) if write => (4, sonora::RAM_WRITE_WAITS),
            Region::Ram(_) => (4, sonora::RAM_READ_WAITS),
            Region::Rom => (4, sonora::ROM_WAITS),
            Region::Vram(_) if write => (2, sonora::VRAM_WRITE_WAITS),
            Region::Vram(_) => (2, sonora::VRAM_READ_WAITS),
            Region::CpuId(_) => (4, IO_WAITS),
            // The Apple IIe Card is a 16-bit port (its declaration ROM
            // sits on the odd bytes, byte lanes 1+3).
            Region::IieCard(_) => (2, IO_WAITS),
        }
    }

    /// Wait states of an I/O cycle
    fn io_waits(&self, offset: Address, write: bool) -> Ticks {
        sonora::io_waits(offset, write)
    }

    /// Wait states of a cycle of the video RAM. Writes are posted:
    /// the CPU only waits for the video RAM to finish the previous write.
    /// When the CPU reads more than one 16-bit half, the Sonora reads the
    /// next half in page mode, faster.
    ///
    /// When overclocking, only the wait states of the port are modelled.
    fn vram_waits(&mut self, acc: &BusAccess<Bus68020>, waits: Ticks) -> Ticks {
        if self.native_clock.is_overclocking() {
            return waits;
        }
        let now = self.native_clock.native_ticks() + acc.pending;
        let hold = self.vram_ready.saturating_sub(now);
        if acc.write {
            self.vram_ready = now + hold + sonora::VRAM_WRITE_CYCLE;
            return waits + hold;
        }
        let waits = match self.vram_next_half.take() {
            Some(addr) if addr == acc.addr => sonora::VRAM_NEXT_HALF_READ_WAITS,
            _ => waits,
        };
        let len = 2 - acc.addr % 2;
        if acc.size > len {
            self.vram_next_half = Some(acc.addr + len);
        }
        waits + hold
    }

    /// Runs a bus cycle on a port of `width` bytes
    fn cycle(
        &mut self,
        acc: BusAccess<Bus68020>,
        (width, waits): (u32, Ticks),
        read: impl FnOnce(&mut Self, Address, u32) -> Option<u32>,
        write: impl FnOnce(&mut Self, Address, u32, u32) -> Option<()>,
    ) -> BusResponse {
        match width {
            1 => bus_access_byte(acc, waits, self, read, write),
            2 => bus_access_word(acc, waits, self, read, write),
            _ => bus_access_long(acc, waits, self, read, write),
        }
    }

    /// I/O cycles take native time: when overclocking, they start on a
    /// native tick and last a native bus cycle plus the wait states of the
    /// port
    fn io_cycle(
        &mut self,
        acc: BusAccess<Bus68020>,
        (width, waits): (u32, Ticks),
        offset: Address,
    ) -> BusResponse {
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        let waits = self.native_clock.scale_dev_waits(w, waits);
        self.cycle(
            acc,
            (width, waits),
            move |s, _, len| s.io_read_bytes(offset, len),
            move |s, _, len, val| s.io_write_bytes(offset, len, val),
        )
    }

    fn ignore_write(&mut self, _addr: Address, _len: u32, _val: u32) -> Option<()> {
        Some(())
    }

    fn rom_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        let idx = addr as usize & self.rom_mask;
        if let Some(bytes) = self.rom.get(idx..idx + len as usize) {
            return Some(be_bytes(bytes));
        }
        (addr..addr + len).try_fold(0, |v, a| Some((v << 8) | u32::from(self.read_rom(a)?)))
    }

    /// Index in the RAM of `len` bytes of the RAM area at `addr`, if they
    /// are all in the RAM and follow each other there
    fn ram_range(&self, addr: Address, len: u32) -> Option<usize> {
        let idx = self.ram_index(addr)?;
        (self.ram_index(addr + len - 1)? == idx + len as usize - 1).then_some(idx)
    }

    fn ram_read(&self, addr: Address, len: u32) -> Option<u32> {
        if let Some(idx) = self.ram_range(addr, len) {
            return Some(be_bytes(&self.ram[idx..idx + len as usize]));
        }
        Some((addr..addr + len).fold(0, |v, a| (v << 8) | u32::from(self.read_ram_byte(a))))
    }

    fn ram_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        if let Some(idx) = self.ram_range(addr, len) {
            self.ram[idx..idx + len as usize]
                .copy_from_slice(&val.to_be_bytes()[4 - len as usize..]);
            return Some(());
        }
        for a in addr..addr + len {
            self.write_ram_byte(a, (val >> (8 * (addr + len - 1 - a))) as Byte);
        }
        Some(())
    }

    fn vram_read(&self, offset: usize, len: u32) -> Option<u32> {
        Some(
            (offset..offset + len as usize).fold(0, |v, o| (v << 8) | u32::from(self.read_vram(o))),
        )
    }

    fn vram_write(&mut self, offset: usize, len: u32, val: u32) -> Option<()> {
        for o in offset..offset + len as usize {
            self.write_vram(o, (val >> (8 * (offset + len as usize - 1 - o))) as Byte);
        }
        Some(())
    }

    fn io_read_bytes(&mut self, offset: Address, len: u32) -> Option<u32> {
        (offset..offset + len).try_fold(0, |v, o| Some((v << 8) | u32::from(self.read_io(o)?)))
    }

    fn io_write_bytes(&mut self, offset: Address, len: u32, val: u32) -> Option<()> {
        for o in offset..offset + len {
            self.write_io(o, (val >> (8 * (offset + len - 1 - o))) as Byte)?;
        }
        Some(())
    }

    /// VIA 1 runs at C16M / 20 and its accesses are synchronous with its
    /// clock, as on the Mac II: one starts on a VIA tick and lasts a VIA
    /// clock cycle. The CPU is released at about 0.7 of it (BusTiming on a real
    /// LC III).
    fn via_cycle(&mut self, acc: BusAccess<Bus68020>, offset: Address) -> BusResponse {
        match self.native_clock.via_sync(&acc, sonora::VIA_RELEASE) {
            DeviceCycle::Stall(n) => BusResponse::Stall(n),
            DeviceCycle::Access(w) => bus_access_byte(
                acc,
                w,
                self,
                move |s, _, len| s.io_read_bytes(offset, len),
                move |s, _, len, val| s.io_write_bytes(offset, len, val),
            ),
        }
    }

    /// The Sonora holds an SCC cycle off until some time has passed since the
    /// start of the previous one (BusTiming on a real LC III)
    fn scc_cycle(&mut self, acc: BusAccess<Bus68020>, offset: Address) -> BusResponse {
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        // When overclocking, acc.pending is 0 after device_sync(), and native
        // ticks are CPU ticks otherwise
        let now = self.native_clock.native_ticks() + acc.pending;
        let hold = self.scc_ready.saturating_sub(now);
        self.scc_ready = now + hold + sonora::SCC_INTERVAL;
        let w = self
            .native_clock
            .scale_dev_waits(w, self.io_waits(offset, acc.write) + hold);
        bus_access_byte(
            acc,
            w,
            self,
            move |s, _, len| s.io_read_bytes(offset, len),
            move |s, _, len, val| s.io_write_bytes(offset, len, val),
        )
    }

    /// In ISM mode, the Sonora holds a SWIM cycle off until some time has
    /// passed since the start of the previous one (BusTiming on a real LC III),
    /// and at least the time the ISM needs between two accesses
    /// (Swim::ism_recovery()).
    /// When overclocking, the accesses are also held as far apart as
    /// Swim::hold() needs.
    fn swim_cycle(&mut self, acc: BusAccess<Bus68020>, offset: Address) -> BusResponse {
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        // When overclocking, acc.pending is 0 after device_sync(), and native
        // ticks are CPU ticks otherwise
        let now = self.native_clock.native_ticks() + acc.pending;
        let hold = self
            .swim_ready
            .saturating_sub(now)
            .max(self.swim.hold(&self.native_clock));
        // FCLK is C16M
        let recovery = (self.swim.ism_recovery() * self.bus_frequency()).div_ceil(C16M_FREQUENCY);
        if recovery > 0 {
            self.swim_ready = now + hold + recovery.max(sonora::SWIM_INTERVAL);
        }
        let w = self
            .native_clock
            .scale_dev_waits(w, self.io_waits(offset, acc.write) + hold);
        bus_access_byte(
            acc,
            w,
            self,
            move |s, _, len| s.io_read_bytes(offset, len),
            move |s, _, len, val| s.io_write_bytes(offset, len, val),
        )
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

    /// Offset in the RAM of the byte at physical address `addr`, if it is in RAM
    fn inspect_ram_offset(&self, addr: Address) -> Option<usize> {
        match self.decode(addr)? {
            Region::Ram(_) if self.overlay => None,
            Region::Ram(addr) => self.ram_index(addr),
            _ => None,
        }
    }

    /// Updates the mouse position in absolute mode, given the current value
    /// of RawMouse. Returns what the low memory globals of the OS should hold
    /// for it, if they should change.
    pub fn mouse_update_abs(
        &mut self,
        x: u16,
        y: u16,
        rawmouse: (u16, u16),
    ) -> Option<AbsoluteMouse> {
        if self.mouse_mode != MouseMode::Absolute {
            return None;
        }

        if !self.mouse_ready && rawmouse != (15, 15) {
            // Wait until the boot process has initialized the mouse position so we don't
            // interfere with the memory test.
            return None;
        }
        self.mouse_ready = true;

        // Report updated mouse coordinates to OS. The current (RawMouse) and
        // new (MTemp) positions are the same, as the LC III ROM takes any
        // difference between them for a relative movement.
        Some(AbsoluteMouse {
            mtemp: (x, y),
            rawmouse: (x, y),
        })
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

impl<TRenderer> Bus<Address, Byte> for LcIIIBus<TRenderer>
where
    TRenderer: Renderer,
{
    type Cpu = Bus68020;

    /// The address is decoded once per bus cycle. Every device is as wide
    /// as its port, so the device callbacks get the address of the cycle.
    fn access(&mut self, acc: BusAccess<Bus68020>) -> Result<BusResponse> {
        let resp = match self.decode(acc.addr) {
            // Everything but RAM and ROM sees the devices up to date, as they are ticked
            Some(Region::Vram(_) | Region::CpuId(_) | Region::IieCard(_)) if acc.pending > 0 => {
                BusResponse::Stall(0)
            }
            // During the boot overlay, reads of the RAM area go to the ROM
            Some(Region::Ram(_)) if self.overlay && !acc.write => {
                let port = self.port(&Region::Rom, false);
                self.cycle(acc, port, Self::rom_read, Self::ignore_write)
            }
            Some(region @ Region::Ram(addr)) => self.cycle(
                acc,
                self.port(&region, acc.write),
                move |s, _, len| s.ram_read(addr, len),
                move |s, _, len, val| s.ram_write(addr, len, val),
            ),
            Some(region @ Region::Rom) => self.cycle(
                acc,
                self.port(&region, acc.write),
                |s, addr, len| {
                    s.overlay = false;
                    s.rom_read(addr, len)
                },
                Self::ignore_write,
            ),
            Some(Region::Io(offset @ 0x0_0000..=0x0_1FFF)) => self.via_cycle(acc, offset),
            Some(Region::Io(offset @ 0x0_4000..=0x0_5FFF)) => self.scc_cycle(acc, offset),
            Some(Region::Io(offset @ 0x1_6000..=0x1_7FFF)) => self.swim_cycle(acc, offset),
            Some(region @ Region::Io(offset)) => {
                self.io_cycle(acc, self.port(&region, acc.write), offset)
            }
            Some(region @ Region::Vram(offset)) => {
                let (width, waits) = self.port(&region, acc.write);
                let waits = self.vram_waits(&acc, waits);
                self.cycle(
                    acc,
                    (width, waits),
                    move |s, _, len| s.vram_read(offset, len),
                    move |s, _, len, val| s.vram_write(offset, len, val),
                )
            }
            Some(region @ Region::CpuId(n)) => self.cycle(
                acc,
                self.port(&region, acc.write),
                move |_, _, len| {
                    Some(be_bytes(
                        &sonora::CPU_ID_LCIII.to_be_bytes()[n..][..len as usize],
                    ))
                },
                Self::ignore_write,
            ),
            Some(region @ Region::IieCard(offset)) => self.cycle(
                acc,
                self.port(&region, acc.write),
                move |s, _, len| s.iie_read(offset, len),
                move |s, _, len, val| s.iie_write(offset, len, val),
            ),
            None => BusResponse::BusError,
        };

        if resp == BusResponse::BusError {
            let rw = if acc.write { "write" } else { "read" };
            warn!("Bus error ({}) at unmapped address: {:08X}", rw, acc.addr);
        }
        Ok(resp)
    }

    fn get_mask(&self) -> Address {
        0xFFFFFFFF
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
        if hard {
            // Clear RAM
            self.ram.fill(0);
            self.disable_memtest();
        }

        self.scc = Scc::new(RTXC_FREQUENCY);
        self.via.reset();
        self.swim.reset();
        self.swim.sel = self.via.pa().headsel();
        self.pseudovia.reset();
        self.sound.reset();
        self.overlay = true;
        self.mouse_ready = false;
        // The card is an independent computer: only a hard reset (power
        // on) resets it.  The host reloads the firmware after.
        if hard && let Some(card) = &mut self.iie_card {
            card.reset(true);
        }
        Ok(false)
    }

    fn localtalk_poll_needed(&self) -> bool {
        self.scc.localtalk_poll_needed(SccCh::B)
    }
}

impl<TRenderer> Tickable for LcIIIBus<TRenderer>
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
            if step.native == 0 && !step.via_tick {
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

                // 60.15 Hz interrupt, generated by the Sonora
                self.tick60_clock += 1;
                if self.tick60_clock >= TICK60_PERIOD {
                    self.tick60_clock = 0;
                    self.via.set_line(ViaLine::CA1, LineState::Pulse);
                }
            }
        }

        // The Apple IIe Card runs in its own clock domain, ticked at C16M.
        let bus_frequency = self.bus_frequency();
        if let Some(card) = &mut self.iie_card {
            let ticks = native_ticks * C16M_FREQUENCY / bus_frequency;
            card.tick(ticks, ())?;
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

        if self.video.tick(
            native_ticks,
            self.bus_frequency(),
            self.video_regs.frame_period(),
        ) {
            if self.video.renderer.as_ref().unwrap().frame_wanted() {
                match self.video_regs.video_mode() {
                    Some(mode) => self.video.render(&mode)?,
                    None => self.video.blank()?,
                }
            }
            self.pseudovia.set_vblank();
        }

        // Sound. Queuing the samples paces the emulation in the Accurate mode.
        self.sound_clock += native_ticks * C16M_FREQUENCY;
        while self.sound_clock >= self.sound.sample_period() * self.bus_frequency() {
            self.sound_clock -= self.sound.sample_period() * self.bus_frequency();

            // The Apple IIe Card's speaker is mixed in after the DFAC. The
            // level is a guess; the sound filter takes its DC out.
            let speaker = self
                .iie_card
                .as_mut()
                .map_or(0.0, |card| card.take_speaker_level() * IIE_SPEAKER_GAIN);
            self.sound
                .tick(self.speed == EmulatorSpeed::Accurate, speaker)?;
        }
        self.pseudovia.set_asc_irq(self.sound.irq());

        self.scsi.tick(native_ticks, ctx)?;
        self.pseudovia
            .set_scsi(self.scsi.get_irq(), self.scsi.get_drq());
        self.swim.tick(native_ticks, ())?;
        self.scc.tick(native_ticks, ctx.bus_frequency);

        Ok(ticks)
    }
}

impl<TRenderer> IrqSource for LcIIIBus<TRenderer>
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

impl<TRenderer> InspectableBus<Address, Byte> for LcIIIBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        match self.decode(addr)? {
            Region::Ram(_) if self.overlay => self.read_rom(addr),
            Region::Ram(addr) => self.ram_index(addr).map(|idx| self.ram[idx]),
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

    fn inspect_read_bulk(&mut self, addr: Address, out: &mut [Byte]) -> InspectRun {
        // The banks of RAM are at least this size, and so are the regions
        const BLOCK: Address = 1 << 16;
        let max = out.len().min((BLOCK - (addr & (BLOCK - 1))) as usize);
        if let Some(idx) = self.inspect_ram_offset(addr) {
            let last = addr.wrapping_add(max as Address - 1);
            // The block may not be contiguous in RAM, or may end with the RAM
            if self.inspect_ram_offset(last) == Some(idx + max - 1) {
                out[..max].copy_from_slice(&self.ram[idx..idx + max]);
                return InspectRun::Data(max);
            }
        }
        inspect_read_bytes(self, addr, out, BLOCK)
    }

    fn inspect_generation(&self) -> u32 {
        u32::from(self.overlay) | (u32::from(self.pseudovia.exp().0) << 1)
    }
}

impl<TRenderer> Debuggable for LcIIIBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_group;
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
            match &self.iie_card {
                Some(card) => dbgprop_nest!("Apple IIe Card", card),
                None => dbgprop_group!("Apple IIe Card (absent)", Vec::new()),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    /// Seconds counted by the Egret, which is clocked like the VIA, while the
    /// bus runs for a second less `less` cycles
    fn clock_seconds(overclock: Option<u64>, less: Ticks) -> u32 {
        let mut bus = LcIIIBus::new(
            MacModel::LCIII,
            &vec![0; 512 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MouseMode::Disabled,
            None,
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
        assert_eq!(clock_seconds(None, 0), 1);
        assert_eq!(clock_seconds(None, 20), 0);
    }

    /// The LC III's 24-bit MMU table maps the 24-bit $E0 0000 to
    /// $FE00 0000 and $FC 0000 (the IIe app's $50FC 0000 base) to
    /// $FEE0 0000: both reach the register block, and the declaration
    /// ROM sits at the top of each 1 MB.
    #[test]
    fn iie_card_lciii_slot_aliases() {
        let mut bus = LcIIIBus::new(
            MacModel::LCIII,
            &vec![0; 1024 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MouseMode::Disabled,
            None,
            None,
            None,
            Some(IieCardSettings::default()),
        );
        fn read(bus: &mut LcIIIBus<NullRenderer>, addr: Address) -> u32 {
            match bus
                .access(BusAccess::<Bus68020>::read(addr, 1, 0, 0))
                .unwrap()
            {
                BusResponse::Ack { data, .. } => data,
                r => panic!("{addr:08X}: {r:?}"),
            }
        }

        let _ = bus
            .access(BusAccess::<Bus68020>::write(0xFE00_0018, 1, 0x41, 0, 0))
            .unwrap();
        for base in [0xFE00_0000, 0xFEE0_0000, 0xEEE0_0000] {
            assert_eq!(read(&mut bus, base + 0x18), 0xC1, "registers at {base:08X}");
        }
        // The declaration ROM's last byte (byte lane 3), at the top of
        // each 1 MB.
        let top = read(&mut bus, 0xFEFF_FFFF);
        assert_eq!(read(&mut bus, 0xFE0F_FFFF), top);
        assert_eq!(read(&mut bus, 0xFEEF_FFFF), top);
        // $50FC 0000 is not the PDS on the LC III.
        assert_ne!(read(&mut bus, 0x50FC_0018), 0xC1);
    }
}
