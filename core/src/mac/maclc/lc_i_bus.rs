//! Bus of the Macintosh LC and LC II, around the V8 gate array

use std::thread;
use std::time::{Duration, Instant};

use super::pds_iie::{IieCardSettings, PdsIIeCard};
use super::pseudovia::PseudoVia;
use super::v8;
use super::via::Via;
use super::video::{self, Video};
use crate::bus::access::{
    Bus68020, BusAccess, BusResponse, CpuBus, bus_access_byte, bus_access_long, bus_access_word,
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
#[cfg(feature = "ethernet")]
use crate::mac::ethernet::appl_lc_sonic::ApplLcSonic;
#[cfg(feature = "ethernet")]
use crate::mac::ethernet::sonic::SonicDma;
use crate::mac::rtc::Rtc;
use crate::mac::scc::{RTXC_FREQUENCY, Scc, SccCh};
use crate::mac::scsi::controller::ScsiController;
use crate::mac::sound::v8_sonora::{V8Sound, V8Variant};
use crate::mac::swim::Swim;
use crate::mac::via_common::{LineState, ViaLine};
use crate::mac::{AbsoluteMouse, MacModel, MacMonitor, WARM_START_SIGNATURE};
#[cfg(feature = "ethernet")]
use crate::net::EthernetPort;
use crate::renderer::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, Renderer, null_audio_sink,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};

use anyhow::Result;
use log::*;
use serde::{Deserialize, Serialize};

/// Frequency of the bus, without overclocking: C16M, which the CPU runs at
const BUS_FREQUENCY: Ticks = C16M_FREQUENCY;

/// Wait states of an I/O bus cycle, when not measured
pub(super) const IO_WAITS: Ticks = 1;

/// Bytes as a big endian number
fn be_bytes(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |v, &b| (v << 8) | u32::from(b))
}

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
    /// The card in PDS slot $E, with the offset in the slot window
    Pds(Address),
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct LcIBus<TRenderer: Renderer> {
    /// The currently emulated Macintosh model
    model: MacModel,

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
    /// Native time the V8 can start its next RAM cycle
    v8_ready: Ticks,
    /// Native time the V8 can take the next RAM write
    v8_write_ready: Ticks,
    /// Address and native start time of the second half of a long RAM read,
    /// which the V8 started before the end of the first half
    v8_next_half: Option<(Address, Ticks)>,
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
    /// Apple Ethernet LC card in the PDS slot, when installed
    #[cfg(feature = "ethernet")]
    ethernet_card: Option<Box<ApplLcSonic>>,
    /// Changes when the Ethernet card's SONIC writes RAM, for inspect_generation()
    #[cfg(feature = "ethernet")]
    #[serde(skip)]
    dma_generation: u32,

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

impl<TRenderer> LcIBus<TRenderer>
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
        const MB: usize = 1024 * 1024;

        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());
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
            .filter(|m| video::monitors(model).contains(m))
            .unwrap_or_default();
        let vram_size = vram_size
            .filter(|s| video::vram_sizes(model).contains(s))
            .unwrap_or_else(|| video::vram_size_default(model));
        let video = Video::new(renderer, monitor, vram_size);
        let swim = Swim::new(model.fdd_drives(), model.fdd_hd(), BUS_FREQUENCY);
        let mut egret = Egret::default();
        egret.adb_add_device(AdbMouse::new());
        egret.adb_add_device(AdbKeyboard::new());

        let mut bus = Self {
            model,

            rom_mask: rom.len() - 1,
            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            ram_soldered,
            via: Via::new(model),
            egret,
            scc_ready: 0,
            swim_ready: 0,
            v8_ready: 0,
            v8_write_ready: 0,
            v8_next_half: None,
            tick60_clock: 0,
            scc: Scc::new(RTXC_FREQUENCY),
            swim,
            scsi: ScsiController::new(),
            pseudovia: PseudoVia::new(video.sense(), v8::PSEUDOVIA_MASK),
            video,
            sound: V8Sound::new(V8Variant::V8),
            sound_clock: 0,

            iie_card: iie_card.map(|settings| PdsIIeCard::with_settings(&settings)),
            #[cfg(feature = "ethernet")]
            ethernet_card: None,
            #[cfg(feature = "ethernet")]
            dma_generation: 0,

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
        #[cfg(feature = "ethernet")]
        if let Some(card) = &mut self.ethernet_card {
            card.after_deserialize();
        }
    }

    /// Puts an Apple Ethernet LC card in the PDS slot, while the machine is built
    #[cfg(feature = "ethernet")]
    pub fn set_ethernet_card(&mut self, card: ApplLcSonic) {
        self.ethernet_card = Some(Box::new(card));
    }

    /// The Ethernet port of the card in the PDS slot, if it is an Ethernet card
    #[cfg(feature = "ethernet")]
    pub fn ethernet_card_mut(&mut self) -> Option<&mut EthernetPort> {
        self.ethernet_card.as_mut().map(|c| c.port_mut())
    }

    /// Calls `f` with the Ethernet card and the memory that its DMA reaches
    #[cfg(feature = "ethernet")]
    fn with_ethernet_card<R>(
        &mut self,
        f: impl FnOnce(&mut ApplLcSonic, &mut PdsDma<'_, TRenderer>) -> R,
    ) -> Option<R> {
        let mut card = self.ethernet_card.take()?;
        let r = f(&mut card, &mut PdsDma(self));
        self.ethernet_card = Some(card);
        Some(r)
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
        v8::decode(addr, self.pseudovia.b().fc3())
    }

    /// Makes the ROM skip its memory test
    ///
    /// These ROMs do not look for the warm start signature at $CFC but in the
    /// long just below the memory table that they build at the top of the
    /// first RAM bank they find. That is the soldered bank, and the table has
    /// one more entry with a SIMM. The offsets were measured by running the
    /// ROMs with every RAM size.
    fn disable_memtest(&mut self) {
        const MB: usize = 1024 * 1024;

        let simm_size = self.ram.len() - self.ram_soldered;
        // Only 2 MB of the soldered RAM are used with an 8 MB SIMM
        let soldered_size = if self.model == MacModel::LCII && simm_size == 8 * MB {
            2 * MB
        } else {
            self.ram_soldered
        };
        let below_top = if simm_size == 0 { 0x18 } else { 0x20 };
        let index = simm_size + soldered_size - below_top;
        info!("Skipping memory test");
        self.ram[index..index + 4].copy_from_slice(&WARM_START_SIGNATURE.1.to_be_bytes());
    }

    /// Translates an address in the RAM area to an index in the RAM, if
    /// there is RAM at the address
    fn ram_index(&self, addr: Address) -> Option<usize> {
        let exp = self.pseudovia.exp();
        let simm_size = self.ram.len() - self.ram_soldered;
        v8::ram_index(exp, simm_size, self.ram_soldered, addr)
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
    /// the video RAM
    fn vram_index(&self, offset: usize) -> usize {
        v8::vram_index(self.video.vram_size(), offset)
    }

    fn read_vram(&self, offset: usize) -> Byte {
        self.video.read_vram(self.vram_index(offset))
    }

    fn write_vram(&mut self, offset: usize, val: Byte) {
        self.video.write_vram(self.vram_index(offset), val);
    }

    /// Writes to an I/O device. `offset` is the offset in the I/O area.
    fn write_io(&mut self, offset: Address, val: Byte) -> Option<()> {
        let addr = v8::IO_BASE | offset;
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
            // V8 registers
            0x2_6000..=0x2_7FFF => self.pseudovia.write(addr, val),
            _ => None,
        }
    }

    /// Reads from an I/O device. `offset` is the offset in the I/O area.
    fn read_io(&mut self, offset: Address) -> Option<Byte> {
        let addr = v8::IO_BASE | offset;
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

    /// One cycle of the card in the PDS slot, with the offset in the slot:
    /// without a card, the slot reads open bus and ignores writes.
    fn pds_read(&mut self, offset: Address, len: u32) -> Option<u32> {
        if let Some(card) = &mut self.iie_card {
            return Some(card.slot_read(offset, len));
        }
        #[cfg(feature = "ethernet")]
        if let Some(card) = &mut self.ethernet_card {
            return Some(card.read(offset, len));
        }
        Some(PdsIIeCard::open_bus(len))
    }

    fn pds_write(&mut self, offset: Address, len: u32, val: u32) -> Option<()> {
        if let Some(card) = &mut self.iie_card {
            card.slot_write(offset, len, val);
        }
        #[cfg(feature = "ethernet")]
        self.with_ethernet_card(|card, dma| card.write(offset, len, val, dma));
        Some(())
    }

    /// Width in bytes and wait states of the port answering a region.
    ///
    /// The RAM and video RAM data buses of the V8 are 16-bit, the ROM is
    /// 32-bit. The V8 takes RAM and video RAM writes as a 32-bit port, and
    /// writes them in 16-bit halves after. The I/O devices are on D31-D24, as
    /// an 8-bit port.
    ///
    /// The wait states of the V8 RAM are those of a cycle that starts on a
    /// slot: v8_slot_waits() adds the rest.
    fn port(&self, region: &Region, write: bool) -> (u32, Ticks) {
        match region {
            Region::Io(offset) => (1, self.io_waits(*offset, write)),
            Region::Ram(_) | Region::Vram(_) if write => (4, 0),
            Region::Ram(_) | Region::Vram(_) => (2, v8::RAM_WAITS),
            Region::Rom => (4, v8::ROM_WAITS),
            // The Apple IIe Card is a 16-bit port (its declaration ROM
            // sits on the odd bytes, byte lanes 1+3).
            Region::Pds(_) => (2, IO_WAITS),
        }
    }

    /// Wait states of an I/O cycle
    fn io_waits(&self, offset: Address, write: bool) -> Ticks {
        v8::io_waits(offset, write)
    }

    /// Wait states for a cycle of the V8 RAM, of its registers or of the
    /// ASC to start on the next free slot. RAM writes are posted: the CPU
    /// only waits for the V8 to take the write, which it then does in one
    /// cycle per 16-bit half, one slot apart.
    ///
    /// When overclocking, the slots are not modelled.
    fn v8_slot_waits(&mut self, acc: &BusAccess<Bus68020>, ram_write: bool) -> Ticks {
        if self.native_clock.is_overclocking() {
            return 0;
        }
        let now = self.native_clock.native_ticks() + acc.pending;
        let slot = |t: Ticks| (t + v8::CYCLE_SLOT - 1) & !(v8::CYCLE_SLOT - 1);
        if !ram_write {
            let start = slot(now.max(self.v8_ready));
            self.v8_ready = start + v8::RAM_CYCLE;
            return start - now;
        }
        let hold = self.v8_write_ready.saturating_sub(now);
        let len = acc.size.min(4 - (acc.addr & 3));
        let second_half = Ticks::from((acc.addr + len - 1) / 2 - acc.addr / 2);
        let last = slot((now + hold).max(self.v8_ready)) + second_half * v8::CYCLE_SLOT;
        self.v8_ready = last + v8::RAM_CYCLE;
        self.v8_write_ready = last;
        hold
    }

    /// Wait states of a read of the V8 RAM or video RAM. When the CPU reads
    /// more than one 16-bit half, the V8 starts the next half on the next
    /// slot, before the end of this cycle.
    fn v8_ram_read_waits(&mut self, acc: &BusAccess<Bus68020>, waits: Ticks) -> Ticks {
        if self.native_clock.is_overclocking() {
            return waits;
        }
        match self.v8_next_half.take() {
            Some((addr, start)) if addr == acc.addr => {
                let now = self.native_clock.native_ticks() + acc.pending;
                self.v8_ready = start + v8::RAM_CYCLE;
                self.v8_ready.saturating_sub(now + Bus68020::CYCLE)
            }
            _ => {
                let waits = waits + self.v8_slot_waits(acc, false);
                let len = 2 - acc.addr % 2;
                if acc.size > len {
                    let start = self.v8_ready - v8::RAM_CYCLE;
                    self.v8_next_half = Some((acc.addr + len, start + v8::CYCLE_SLOT));
                }
                waits
            }
        }
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
        let waits = match offset {
            0x1_4000..=0x1_5FFF | 0x2_6000..=0x2_7FFF => waits + self.v8_slot_waits(&acc, false),
            _ => waits,
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
    /// clock cycle. The CPU is released near the end of it (BusTiming on a real
    /// LC II).
    fn via_cycle(&mut self, acc: BusAccess<Bus68020>, offset: Address) -> BusResponse {
        match self.native_clock.via_sync(&acc, v8::VIA_RELEASE) {
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

    /// The V8 holds an SCC cycle off until some time has passed since the
    /// start of the previous one (BusTiming on a real LC II)
    fn scc_cycle(&mut self, acc: BusAccess<Bus68020>, offset: Address) -> BusResponse {
        let w = match self.native_clock.device_sync(&acc) {
            DeviceCycle::Stall(n) => return BusResponse::Stall(n),
            DeviceCycle::Access(w) => w,
        };
        // When overclocking, acc.pending is 0 after device_sync(), and native
        // ticks are CPU ticks otherwise
        let now = self.native_clock.native_ticks() + acc.pending;
        let hold = self.scc_ready.saturating_sub(now);
        self.scc_ready = now + hold + v8::SCC_INTERVAL;
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

    /// In ISM mode, the V8 holds a SWIM cycle off until some time has passed
    /// since the start of the previous one (BusTiming on a real LC II), and at
    /// least the time the ISM needs between two accesses
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
            self.swim_ready = now + hold + recovery.max(v8::SWIM_INTERVAL);
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

impl<TRenderer> Bus<Address, Byte> for LcIBus<TRenderer>
where
    TRenderer: Renderer,
{
    type Cpu = Bus68020;

    /// The address is decoded once per bus cycle. Every device is as wide
    /// as its port, so the device callbacks get the address of the cycle.
    fn access(&mut self, acc: BusAccess<Bus68020>) -> Result<BusResponse> {
        let resp = match self.decode(acc.addr) {
            // Everything but RAM and ROM sees the devices up to date, as they are ticked
            Some(Region::Vram(_) | Region::Pds(_)) if acc.pending > 0 => BusResponse::Stall(0),
            // During the boot overlay, reads of the RAM area go to the ROM
            Some(Region::Ram(_)) if self.overlay && !acc.write => {
                let port = self.port(&Region::Rom, false);
                self.cycle(acc, port, Self::rom_read, Self::ignore_write)
            }
            Some(region @ Region::Ram(addr)) => {
                let (width, waits) = self.port(&region, acc.write);
                let waits = if acc.write {
                    waits + self.v8_slot_waits(&acc, true)
                } else {
                    self.v8_ram_read_waits(&acc, waits)
                };
                self.cycle(
                    acc,
                    (width, waits),
                    move |s, _, len| s.ram_read(addr, len),
                    move |s, _, len, val| s.ram_write(addr, len, val),
                )
            }
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
                let waits = if acc.write {
                    waits + self.v8_slot_waits(&acc, true)
                } else {
                    self.v8_ram_read_waits(&acc, waits)
                };
                self.cycle(
                    acc,
                    (width, waits),
                    move |s, _, len| s.vram_read(offset, len),
                    move |s, _, len, val| s.vram_write(offset, len, val),
                )
            }
            Some(region @ Region::Pds(offset)) => self.cycle(
                acc,
                self.port(&region, acc.write),
                move |s, _, len| s.pds_read(offset, len),
                move |s, _, len, val| s.pds_write(offset, len, val),
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
        #[cfg(feature = "ethernet")]
        if let Some(card) = &mut self.ethernet_card {
            card.reset();
        }
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

impl<TRenderer> Tickable for LcIBus<TRenderer>
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

                // 60.15 Hz interrupt, generated by the V8
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

        // The SONIC of an Ethernet card raises the slot interrupt of the gate array
        #[cfg(feature = "ethernet")]
        if let Some(irq) = self.with_ethernet_card(|card, dma| {
            card.tick(native_ticks, bus_frequency, dma);
            card.irq()
        }) {
            self.pseudovia.set_pds_irq(irq);
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
        if self.video.tick(
            native_ticks,
            self.bus_frequency(),
            v8::frame_period(monitor),
        ) {
            if self.video.renderer.as_ref().unwrap().frame_wanted() {
                let mode = v8::video_mode(monitor, self.pseudovia.exp(), self.pseudovia.monp());
                self.video.render(&mode)?;
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

impl<TRenderer> IrqSource for LcIBus<TRenderer>
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

impl<TRenderer> InspectableBus<Address, Byte> for LcIBus<TRenderer>
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
        #[cfg(feature = "ethernet")]
        let dma = self.dma_generation << 9;
        #[cfg(not(feature = "ethernet"))]
        let dma = 0;
        u32::from(self.overlay) | (u32::from(self.pseudovia.exp().0) << 1) | dma
    }
}

impl<TRenderer> Debuggable for LcIBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_group;
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        #[allow(unused_mut)]
        let mut props = vec![
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
        ];
        #[cfg(feature = "ethernet")]
        if let Some(card) = &self.ethernet_card {
            props.push(dbgprop_nest!("Apple Ethernet LC card", card));
        }
        props
    }
}

/// What the SONIC of a PDS card reaches: the RAM, through the gate array's
/// address decode. The model doesn't count the time its DMA cycles take on
/// the bus.
#[cfg(feature = "ethernet")]
struct PdsDma<'a, TRenderer: Renderer>(&'a mut LcIBus<TRenderer>);

#[cfg(feature = "ethernet")]
impl<TRenderer: Renderer> SonicDma for PdsDma<'_, TRenderer> {
    fn read_byte(&mut self, addr: Address) -> u8 {
        match self.0.decode(addr) {
            Some(Region::Ram(a)) => self.0.read_ram_byte(a),
            _ => LcIBus::<TRenderer>::OPENBUS,
        }
    }

    fn write_byte(&mut self, addr: Address, val: u8) {
        if let Some(Region::Ram(a)) = self.0.decode(addr) {
            self.0.write_ram_byte(a, val);
            // Memory notifiers only see the CPU's writes: a new DMA generation
            // makes them read their ranges again
            self.0.dma_generation = self.0.dma_generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::NullRenderer;

    /// Seconds counted by the Egret, which is clocked like the VIA, while the
    /// bus runs for a second less `less` cycles
    fn clock_seconds(overclock: Option<u64>, less: Ticks) -> u32 {
        let mut bus = LcIBus::new(
            MacModel::LC,
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
        // Overclocked
        assert_eq!(clock_seconds(Some(32_000_000), 0), 1);
        assert_eq!(clock_seconds(Some(32_000_000), 20), 0);
    }

    /// The Apple IIe Card register block answers from its windows in
    /// PDS slot $E space and the pseudo-slot I/O space, and bytes
    /// outside it read open bus.
    #[test]
    fn iie_card_register_block_in_slot_space() {
        let mut bus = LcIBus::new(
            MacModel::LC,
            &vec![0; 512 * 1024],
            NullRenderer::new(0, 0).unwrap(),
            MouseMode::Disabled,
            None,
            None,
            None,
            Some(IieCardSettings::default()),
        );

        // The card is a 16-bit port
        let ack = |data| BusResponse::ack(2, data, IO_WAITS);

        // Pseudo-slot I/O window ($50FC 0000 in the 32-bit map): the
        // trap-code register reads 0 when not trapped.
        let resp = bus
            .access(BusAccess::<Bus68020>::read(0xFC_0000, 1, 0))
            .unwrap();
        assert_eq!(resp, ack(0));

        // Keyboard register through the same window: write $18 high
        // byte, read it back (key | strobe).
        let _ = bus
            .access(BusAccess::<Bus68020>::write(0xFC_0018, 1, 0x41, 0))
            .unwrap();
        let resp = bus
            .access(BusAccess::<Bus68020>::read(0xFC_0018, 1, 0))
            .unwrap();
        assert_eq!(resp, ack(0xC1));

        // The same register block at the bottom of the 24-bit slot
        // window ($E0 0000).
        let resp = bus
            .access(BusAccess::<Bus68020>::read(0xE0_0018, 1, 0))
            .unwrap();
        assert_eq!(resp, ack(0xC1));

        // Outside the register block reads open bus.
        let resp = bus
            .access(BusAccess::<Bus68020>::read(0xFC_0100, 1, 0))
            .unwrap();
        assert_eq!(resp, ack(0xFF));
        let resp = bus
            .access(BusAccess::<Bus68020>::read(0xE8_0018, 1, 0))
            .unwrap();
        assert_eq!(resp, ack(0xFF));
    }
}
