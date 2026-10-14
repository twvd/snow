use std::ops::Range;
use std::thread;
use std::time::{Duration, Instant};

use super::audio::AudioState;
use super::video::Video;
use crate::bus::access::{Bus68000, BusAccess, BusResponse, bus_access_word};
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::{EmuContext, MouseMode};
use crate::keymap::KeyEvent;
use crate::mac::MacModel;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::macii::bus::DEFAULT_BUS_SPEED;
use crate::mac::rtc::Rtc;
use crate::mac::scc::{Scc, SccCh};
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::Swim;
use crate::mac::swim::drive::DriveType;
use crate::mac::via::Via;
use crate::mac::via_common::{LineState, ViaLine};
use crate::renderer::{
    AUDIO_BUFFER_SAMPLES, AUDIO_CHANNELS, AudioProvider, Renderer, null_audio_sink,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, MouseEvent};
use crate::util::take_from_accumulator;

use anyhow::Result;
use bit_set::BitSet;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};
use serde::{Deserialize, Serialize};

/// Size of a RAM page in MacBus::ram_dirty
pub const RAM_DIRTY_PAGESIZE: usize = 256;

/// Size of the blocks of the address map
const MAP_BLOCK_SHIFT: u32 = 17;
const MAP_BLOCKS: usize = 1 << (24 - MAP_BLOCK_SHIFT);

/// What answers in a block of the address map
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    None,
    Ram,
    /// RAM while the boot overlay is on
    RamOverlay,
    Rom,
    /// Reads the open bus
    OpenBus,
    Scsi,
    Scc,
    Iwm,
    Via,
    PhaseRead,
    /// Test software region / extension ROM
    ExtRom,
}

/// Address map: the region of each block, with the boot overlay off and on
type AddressMap = [[Region; MAP_BLOCKS]; 2];

/// A bus cycle being stalled, between the times the CPU presents it
#[derive(Default)]
struct Stalled {
    /// Wait states so far
    waits: Ticks,
    /// VPA bytes synchronized to the E clock
    vpa_bytes: u32,
    /// The E clock has been waited for once for the current VPA byte
    vpa_polled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct CompactMacBus<TRenderer: Renderer> {
    cycles: Ticks,

    /// The currently emulated Macintosh model
    model: MacModel,

    rom: Vec<u8>,
    extension_rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,

    /// RAM pages (RAM_DIRTY_PAGESIZE bytes) written
    pub(crate) ram_dirty: BitSet,

    pub(crate) via: Via,
    pub(crate) scc: Scc,
    pub(crate) video: Video<TRenderer>,

    #[serde(skip)]
    pub(crate) audio: AudioState,

    eclock: Ticks,
    /// Not saved, as a cycle is never in progress between instructions
    #[serde(skip)]
    stalled: Stalled,
    mouse_ready: bool,
    pub(crate) swim: Swim,
    pub(crate) scsi: ScsiController,

    ram_mask: usize,
    rom_mask: usize,

    /// Main video framebuffer address range
    fb_main: Range<Address>,

    /// Alternate video framebuffer address range
    fb_alt: Range<Address>,

    /// Main sound and disk drive PWM address range
    /// Sound is in the upper bytes per 16-bit pair, disk PWM in the lower
    soundbuf_main: Range<usize>,

    /// Alternate sound and disk drive PWM address range
    /// Sound is in the upper bytes per 16-bit pair, disk PWM in the lower
    soundbuf_alt: Range<usize>,

    overlay: bool,

    #[serde(skip, default = "empty_map")]
    map: AddressMap,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    bus_frequency: Ticks,

    /// Last pushed audio sample
    last_audiosample: u8,

    /// Last vblank time (for syncing to video)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for in Video speed mode.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,

    /// Programmer's key pressed
    progkey_pressed: LatchingEvent,

    /// Mouse mode
    mouse_mode: MouseMode,

    /// Early/Plus mouse motion accumulator for X coordinate
    plusmouse_rel_x: i32,

    /// Early/Plus mouse motion accumulator for Y coordinate
    plusmouse_rel_y: i32,

    /// Tracks the last values of the (16-bit) data bus to produce accurate
    /// echoes for open bus reads.
    openbus: [Byte; 2],
}

impl<TRenderer> CompactMacBus<TRenderer>
where
    TRenderer: Renderer,
{
    /// Main sound buffer offset (from end of RAM)
    const SOUND_MAIN_OFFSET: usize = 0x0300;
    /// Alternate sound buffer offset (from end of RAM)
    const SOUND_ALT_OFFSET: usize = 0x5F00;
    /// Size of the sound buffer, in bytes
    const SOUNDBUF_SIZE: usize = 370 * 2;

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
        override_fdd_type: Option<DriveType>,
    ) -> Self {
        let ram_size = ram_size.unwrap_or_else(|| model.ram_size_default());

        let fb_alt_start = ram_size as Address - crate::mac::compact::video::FRAMEBUFFER_ALT_OFFSET;
        let fb_main_start =
            ram_size as Address - crate::mac::compact::video::FRAMEBUFFER_MAIN_OFFSET;
        let sound_alt_start = ram_size - Self::SOUND_ALT_OFFSET;
        let sound_main_start = ram_size - Self::SOUND_MAIN_OFFSET;

        let fdds = if let Some(override_fdd_type) = override_fdd_type {
            vec![override_fdd_type; model.fdd_drives().len()]
        } else {
            model.fdd_drives().to_vec()
        };

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
            via: Via::new(model),
            video: Video::new(renderer),
            audio: AudioState::default(),
            eclock: 0,
            stalled: Stalled::default(),
            scc: Scc::new(),
            swim: Swim::new(&fdds, model.fdd_hd(), 8_000_000),
            scsi: ScsiController::new(),
            mouse_ready: false,

            ram_mask: (ram_size - 1),
            rom_mask: rom.len() - 1,

            fb_main: fb_main_start
                ..(fb_main_start + crate::mac::compact::video::FRAMEBUFFER_SIZE as Address),
            fb_alt: fb_alt_start
                ..(fb_alt_start + crate::mac::compact::video::FRAMEBUFFER_SIZE as Address),

            soundbuf_main: sound_main_start..(sound_main_start + Self::SOUNDBUF_SIZE),
            soundbuf_alt: sound_alt_start..(sound_alt_start + Self::SOUNDBUF_SIZE),

            overlay: true,
            map: build_map(model),
            speed: EmulatorSpeed::Accurate,
            bus_frequency: DEFAULT_BUS_SPEED,
            last_audiosample: 0,
            vblank_time: Instant::now(),
            progkey_pressed: LatchingEvent::default(),
            mouse_mode,
            plusmouse_rel_x: 0,
            plusmouse_rel_y: 0,
            openbus: Default::default(),
        };

        // Disable memory test
        if let Some((addr, value)) = model.disable_memtest() {
            info!("Skipping memory test");
            bus.write_ram(addr, value);
        }

        // Initialize ADB devices
        if model.has_adb() {
            bus.via.adb.add_device(AdbMouse::new());
            bus.via.adb.add_device(AdbKeyboard::new());
        }

        bus
    }

    /// Reinstalls things that can't be serialized and does some updates upon deserialization
    pub fn after_deserialize(&mut self, renderer: TRenderer) {
        self.map = build_map(self.model);
        self.video.renderer = Some(renderer);
        // Make sure we have at least the last frame available
        self.video.render().unwrap();

        // Mark all RAM pages as dirty after deserialization to update memory display
        self.ram_dirty
            .extend(0..(self.ram.len() / RAM_DIRTY_PAGESIZE));
    }

    pub fn model(&self) -> MacModel {
        self.model
    }

    pub fn get_effective_speed(&self) -> f64 {
        self.via.rtc.effective_speed()
    }

    pub fn set_audio_provider(&mut self, provider: &mut dyn AudioProvider) -> Result<()> {
        // Audio sample frequency is tied to monitor's horizontal sync
        // 370 horizontal lines * 60.147 frames/sec = 22.254 KHz
        // Round down to a safe commonly used value (22050), 0.9% off
        let sink = provider
            .create_stream(22050, AUDIO_CHANNELS as u8, AUDIO_BUFFER_SAMPLES as u16)
            .unwrap_or_else(|_| null_audio_sink());
        self.audio.set_sink(sink);

        self.scsi.set_audio_provider(provider)
    }

    fn soundbuf(&mut self) -> &mut [u8] {
        if self.model >= MacModel::SE || self.via.pa().sndpg2() {
            &mut self.ram[self.soundbuf_main.clone()]
        } else {
            &mut self.ram[self.soundbuf_alt.clone()]
        }
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

    /// Ends the boot overlay on an access to the normal location of the ROM
    /// (Mac SE and later)
    fn end_overlay(&mut self, addr: Address, write: bool) {
        let end = if write { 0x0057_FFFF } else { 0x004F_FFFF };
        if self.overlay && self.model >= MacModel::SE && (0x0040_0000..=end).contains(&addr) {
            self.overlay = false;
        }
    }

    fn ram_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        Some((addr..addr + len).fold(0, |v, a| {
            (v << 8) | u32::from(self.ram[a as usize & self.ram_mask])
        }))
    }

    fn ram_write(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        for a in addr..addr + len {
            let val = (val >> (8 * (addr + len - 1 - a))) as Byte;

            // Duplicate framebuffers to video component
            // (writes also go through RAM)
            if self.fb_main.contains(&(a & self.ram_mask as Address)) {
                let offset = ((a & self.ram_mask as Address) - self.fb_main.start) as usize;
                self.video.framebuffers[0][offset] = val;
            }
            if self.fb_alt.contains(&(a & self.ram_mask as Address)) {
                let offset = ((a & self.ram_mask as Address) - self.fb_alt.start) as usize;
                self.video.framebuffers[1][offset] = val;
            }

            let idx = a as usize & self.ram_mask;
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
            self.ram[idx] = val;
        }
        Some(())
    }

    fn ram_write_overlay(&mut self, addr: Address, len: u32, val: u32) -> Option<()> {
        for a in addr..addr + len {
            let idx = ((a as usize) - 0x60_0000) & self.ram_mask;
            self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
            self.ram[idx] = (val >> (8 * (addr + len - 1 - a))) as Byte;
        }
        Some(())
    }

    fn rom_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        Some((addr..addr + len).fold(0, |v, a| {
            let open = self.openbus[(a & 1) as usize];
            (v << 8) | u32::from(*self.rom.get(a as usize & self.rom_mask).unwrap_or(&open))
        }))
    }

    fn ext_rom_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        Some((addr..addr + len).fold(0, |v, a| {
            let open = self.openbus[(a & 1) as usize];
            let rom = self.extension_rom.get((a - 0xF8_0000) as usize);
            (v << 8) | u32::from(*rom.unwrap_or(&open))
        }))
    }

    fn openbus_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        Some((addr..addr + len).fold(0, |v, a| {
            (v << 8) | u32::from(self.openbus[(a & 1) as usize])
        }))
    }

    fn zero_read(&mut self, _addr: Address, _len: u32) -> Option<u32> {
        Some(0)
    }

    fn no_write(&mut self, _addr: Address, _len: u32, _val: u32) -> Option<()> {
        None
    }

    // The byte-wide chips answer on every byte of their range. A word read
    // gets the byte twice, a word write writes its low byte.

    fn scsi_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        self.scsi.read(addr).map(|v| both_bytes(v, len))
    }

    fn scsi_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.scsi.write(addr, val as Byte)
    }

    fn scc_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        self.scc.read(addr >> 1).map(|v| both_bytes(v, len))
    }

    fn scc_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.scc.write(addr >> 1, val as Byte)
    }

    /// Reads of $9FFFF7 and $9FFFF9 adjust the phase
    fn scc_phase_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        if addr & 1 != 0 {
            Some(0)
        } else {
            self.scc_read(addr, len)
        }
    }

    fn iwm_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        self.swim.read(addr).map(|v| both_bytes(v, len))
    }

    fn iwm_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.swim.write(addr, val as Byte)
    }

    fn via_read(&mut self, addr: Address, len: u32) -> Option<u32> {
        self.via.read(addr).map(|v| both_bytes(v, len))
    }

    fn via_write(&mut self, addr: Address, _len: u32, val: u32) -> Option<()> {
        self.via.write(addr, val as Byte)
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        if self.mouse_mode == MouseMode::Disabled {
            return;
        }

        if let Some(b) = button {
            if !self.model.has_adb() {
                // Mouse button through VIA I/O
                self.via.b_in.set_sw(!b);
            } else {
                self.via.adb.event(&AdbEvent::Mouse(MouseEvent {
                    button,
                    rel_movement: None,
                }));
            }
        }

        if relx == 0 && rely == 0 {
            return;
        }

        match self.mouse_mode {
            MouseMode::Absolute => {
                // Handled through mouse_update_abs()
            }
            MouseMode::RelativeHw if !self.model.has_adb() => {
                self.plusmouse_rel_x = self.plusmouse_rel_x.saturating_add(relx.into());
                self.plusmouse_rel_y = self.plusmouse_rel_y.saturating_add(rely.into());
            }
            MouseMode::RelativeHw => {
                // ADB
                self.via.adb.event(&AdbEvent::Mouse(MouseEvent {
                    button: None,
                    rel_movement: Some((relx.into(), rely.into())),
                }));
            }
            MouseMode::Disabled => unreachable!(),
        }
    }

    /// Sets the mouse emulation mode
    pub fn set_mouse_mode(&mut self, mode: MouseMode) {
        self.mouse_mode = mode;
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

    pub fn set_bus_frequency(&mut self, bus_frequency: u64) {
        self.bus_frequency = bus_frequency;
    }

    /// Stalls the cycle for 2 wait cycles
    fn wait2(&mut self, stall: Ticks) -> Option<Ticks> {
        self.stalled.waits += 2;
        Some(stall)
    }

    /// Stall of a bus cycle which has to wait, polled every 2 cycles with
    /// the bus ticked in between, or None once it can go on
    fn stall(&mut self, acc: &BusAccess<Bus68000>) -> Option<Ticks> {
        match acc.addr {
            // RAM, while the memory controller gives it to the video.
            // The first check is against the bus as last ticked.
            // https://www.bigmessowires.com/2011/08/25/68000-interleaved-memory-controller-design/
            0x0000_0000..=0x003F_FFFF => {
                if !self.video.in_blanking_period() && !self.model.ram_interleave_cpu(self.cycles) {
                    return self.wait2(2);
                }
            }
            // VPA: synchronized to the next low edge of the E clock.
            // TODO done for each byte of a word, as the byte interface did,
            // the second one 2 cycles into the bus cycle
            0x00E0_0000..=0x00FF_FFFF => {
                while self.stalled.vpa_bytes < acc.size {
                    if !self.stalled.vpa_polled {
                        self.stalled.vpa_polled = true;
                        return self.wait2(if self.stalled.vpa_bytes > 0 { 4 } else { 2 });
                    }
                    if self.eclock != 0 {
                        return self.wait2(2);
                    }
                    self.stalled.vpa_bytes += 1;
                    self.stalled.vpa_polled = false;
                }
            }
            _ => (),
        }
        None
    }

    /// Programmer's key pressed
    pub fn progkey(&mut self) {
        self.progkey_pressed.set();
    }

    pub fn video_blank(&mut self) -> Result<()> {
        self.video.blank()
    }

    fn plusmouse_tick(&mut self) {
        if self.mouse_mode != MouseMode::RelativeHw
            || (self.plusmouse_rel_x == 0 && self.plusmouse_rel_y == 0)
        {
            return;
        }

        let motion_x = take_from_accumulator(&mut self.plusmouse_rel_x, 1);
        let motion_y = take_from_accumulator(&mut self.plusmouse_rel_y, 1);

        if motion_x != 0 {
            let dcd_a = self.scc.get_dcd(SccCh::A);
            if motion_x > 0 {
                // Moving right
                self.via.b_in.set_mouse_x2(dcd_a);
            } else if motion_x < 0 {
                // Moving left
                self.via.b_in.set_mouse_x2(!dcd_a);
            }
            self.scc.set_dcd(SccCh::A, !dcd_a);
        }

        if motion_y != 0 {
            let dcd_b = self.scc.get_dcd(SccCh::B);
            if motion_y > 0 {
                // Moving up
                self.via.b_in.set_mouse_y2(!dcd_b);
            } else if motion_y < 0 {
                // Moving down
                self.via.b_in.set_mouse_y2(dcd_b);
            }
            self.scc.set_dcd(SccCh::B, !dcd_b);
        }
    }

    /// Dispatches a key event to the keyboard
    pub fn keyboard_event(&mut self, ke: KeyEvent) {
        if !self.model.has_adb() {
            self.via.keyboard.event(ke);
        } else {
            self.via.adb.event(&AdbEvent::Key(ke));
        }
    }

    /// Releases all pressed inputs
    pub fn input_release_all(&mut self) {
        if !self.model.has_adb() {
            self.via.keyboard.release_all();
            self.mouse_update_rel(0, 0, Some(false));
        } else {
            self.via.adb.event(&AdbEvent::ReleaseAll);
        }
    }

    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.via.rtc
    }
}

impl<TRenderer> Bus<Address, Byte> for CompactMacBus<TRenderer>
where
    TRenderer: Renderer,
{
    type Cpu = Bus68000;
    const SIZED: bool = true;

    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    #[inline(always)]
    fn access(&mut self, acc: BusAccess<Bus68000>) -> Result<BusResponse> {
        if let Some(stall) = self.stall(&acc) {
            return Ok(BusResponse::Stall(stall));
        }
        let waits = std::mem::take(&mut self.stalled).waits;

        self.end_overlay(acc.addr, acc.write);
        let region = self.map[usize::from(self.overlay)][(acc.addr >> MAP_BLOCK_SHIFT) as usize];
        let se = self.model >= MacModel::SE;
        let resp = match region {
            Region::Ram => bus_access_word(acc, waits, self, Self::ram_read, Self::ram_write),
            Region::RamOverlay => {
                bus_access_word(acc, waits, self, Self::ram_read, Self::ram_write_overlay)
            }
            Region::Rom => bus_access_word(acc, waits, self, Self::rom_read, Self::no_write),
            Region::OpenBus => {
                bus_access_word(acc, waits, self, Self::openbus_read, Self::no_write)
            }
            Region::Scsi => bus_access_word(acc, waits, self, Self::scsi_read, Self::scsi_write),
            // TODO the SE and Classic SCC, IWM and VIA only take part of
            // their block
            Region::Scc | Region::Via if se && acc.addr & 0x0001_0000 == 0 => BusResponse::BusError,
            Region::Iwm if se && acc.addr < 0x00DF_E1FF => BusResponse::BusError,
            // Phase adjust (ignore)
            Region::Scc
                if self.overlay
                    && !acc.write
                    && (0x009F_FFF6..=0x009F_FFF9).contains(&acc.addr) =>
            {
                bus_access_word(acc, waits, self, Self::scc_phase_read, Self::no_write)
            }
            Region::Scc => bus_access_word(acc, waits, self, Self::scc_read, Self::scc_write),
            Region::Iwm => bus_access_word(acc, waits, self, Self::iwm_read, Self::iwm_write),
            Region::Via => bus_access_word(acc, waits, self, Self::via_read, Self::via_write),
            Region::PhaseRead => bus_access_word(acc, waits, self, Self::zero_read, Self::no_write),
            Region::ExtRom => bus_access_word(acc, waits, self, Self::ext_rom_read, Self::no_write),
            Region::None => BusResponse::BusError,
        };
        let data = match resp {
            BusResponse::Ack { data, .. } => data,
            BusResponse::Stall(_) => unreachable!(),
            BusResponse::BusError if acc.write => {
                warn!(
                    "Write to unimplemented address: {:08X} {:02X}",
                    acc.addr, acc.data
                );
                0
            }
            BusResponse::BusError => {
                warn!("Read from unimplemented address: {:08X}", acc.addr);
                self.openbus_read(acc.addr, acc.size).unwrap_or_default()
            }
        };

        // The data bus keeps the last value on each lane
        let val = if acc.write { acc.data } else { data };
        for a in acc.addr..acc.addr + acc.size {
            self.openbus[(a & 1) as usize] = (val >> (8 * (acc.addr + acc.size - 1 - a))) as Byte;
        }

        if acc.write {
            if self.model <= MacModel::Plus {
                self.overlay = self.via.pa().overlay();
            }

            // Sync values that live in multiple places
            self.swim.sel = self.via.pa().sel();
            self.video.framebuffer_select = self.via.pa().page2();
        }

        Ok(BusResponse::Ack {
            width: 2,
            data,
            waits,
        })
    }

    fn reset(&mut self, hard: bool) -> Result<bool> {
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
        let Via { adb, rtc, .. } = std::mem::replace(&mut self.via, Via::new(self.model));
        self.via.adb = adb;
        self.via.rtc = rtc;
        self.overlay = true;
        self.scc = Scc::new();

        // All compact Macs have RESET and HALT tied together
        Ok(true)
    }
}

impl<TRenderer> Tickable for CompactMacBus<TRenderer>
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

        // Run up to the next E clock edge or blanking edge at a time, where
        // anything below can change
        let mut left = ticks;
        while left > 0 {
            let ticks = left.min(10 - self.eclock).min(self.video.cycles_to_edge());
            left -= ticks;

            self.cycles += ticks;

            self.eclock += ticks;
            while self.eclock >= 10 {
                // The E Clock is roughly 1/10th of the CPU clock
                // TODO ticks when VPA is asserted
                self.eclock -= 10;

                self.via.tick(1, ctx)?;
            }

            // Pixel clock (15.6672 MHz) is roughly 2x CPU speed
            self.video.tick(2 * ticks, ())?;

            // Sync VIA registers
            if self.model <= MacModel::Plus {
                self.via.b_in.set_h4(self.video.in_hblank());
            } else {
                self.swim.intdrive = self.via.pa().drivesel();
            }

            // VBlank interrupt
            if self.video.get_clr_vblank() {
                self.via.set_line(ViaLine::CA1, LineState::Pulse);

                match self.speed {
                    EmulatorSpeed::Video => {
                        // Sync to 60 fps video
                        let frametime = self.vblank_time.elapsed().as_micros() as u64;
                        const DESIRED_FRAMETIME: u64 = 1_000_000 / 60;
                        self.vblank_time = Instant::now();
                        if frametime < DESIRED_FRAMETIME {
                            thread::sleep(Duration::from_micros(DESIRED_FRAMETIME - frametime));
                        }
                    }
                    EmulatorSpeed::FastForward(max_speed) => {
                        // Cap to the requested speedup multiplier
                        let frametime = self.vblank_time.elapsed().as_micros() as u64;
                        let desired_frametime = (1_000_000.0 / (60.0 * max_speed)) as u64;
                        self.vblank_time = Instant::now();
                        if frametime < desired_frametime {
                            thread::sleep(Duration::from_micros(desired_frametime - frametime));
                        }
                    }
                    _ => {}
                }
            }

            // HBlank
            if self.video.get_clr_hblank() {
                // Update floppy drive PWM and send next audio sample
                let scanline = self.video.get_scanline();
                let soundon = self.via.pa().sound() > 0 && !self.via.pb().sndenb();
                let soundbuf = self.soundbuf();
                let pwm = soundbuf[scanline * 2 + 1];
                let audiosample = if soundon { soundbuf[scanline * 2] } else { 0 };

                self.swim.push_pwm(pwm)?;

                // Sample the mouse here for Early/Plus
                //
                // Oscilloscope traces of the SCC show a mouse interrupt can be triggered
                // at a smallest interval of 284us with vigorous mousing going on.
                // This translates into ~6.3 scanlines. To be on the safe side, round up to
                // 8. If the interval is too short, the ROM will not keep up and will
                // not be able to service Y axis movements anymore.
                //
                // From a DCD edge to asserting the interrupt line takes the SCC ~1.5us.
                if scanline.is_multiple_of(8) {
                    self.plusmouse_tick();
                }

                // Emulator will block here to sync to audio frequency
                match self.speed {
                    EmulatorSpeed::Accurate => self.audio.push(audiosample)?,
                    EmulatorSpeed::Dynamic => {
                        if !self.audio.is_silent() || audiosample != self.last_audiosample {
                            self.audio.push(audiosample)?;
                        }
                    }
                    EmulatorSpeed::Uncapped => (),
                    EmulatorSpeed::Video => (),
                    EmulatorSpeed::FastForward(_) => (),
                }
                self.last_audiosample = audiosample;
            }

            self.scsi.tick(ticks, ctx)?;
            self.swim.tick(ticks, ())?;
        }

        Ok(ticks)
    }
}

impl<TRenderer> IrqSource for CompactMacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
        // Programmer's key
        if self.progkey_pressed.get_clear() {
            return Some(4);
        }
        // SCC
        if self.scc.get_irq() {
            return Some(2);
        }
        // VIA IRQs
        if self.via.irq() {
            return Some(1);
        }
        // SCSI IRQs
        if self.model >= MacModel::SE && self.scsi.get_irq() && !self.via.pb().scsi_int() {
            return Some(1);
        }

        None
    }
}

impl<TRenderer> InspectableBus<Address, Byte> for CompactMacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn inspect_read(&mut self, addr: Address) -> Option<Byte> {
        // Everything up to 0x800000 is safe (RAM/ROM only)
        if addr >= 0x80_0000 {
            return None;
        }
        self.end_overlay(addr, false);
        let val = match self.map[usize::from(self.overlay)][(addr >> MAP_BLOCK_SHIFT) as usize] {
            Region::Ram | Region::RamOverlay => self.ram_read(addr, 1),
            Region::Rom => self.rom_read(addr, 1),
            Region::OpenBus => self.openbus_read(addr, 1),
            Region::Scsi => self.scsi_read(addr, 1),
            _ => None,
        };
        val.map(|v| v as Byte)
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        // Everything up to 0x800000 is safe (RAM/ROM only)
        if addr >= 0x80_0000 {
            return None;
        }
        self.end_overlay(addr, true);
        let val = val.into();
        match self.map[usize::from(self.overlay)][(addr >> MAP_BLOCK_SHIFT) as usize] {
            Region::Ram => self.ram_write(addr, 1, val),
            Region::RamOverlay => self.ram_write_overlay(addr, 1, val),
            Region::Scsi => self.scsi_write(addr, 1, val),
            _ => None,
        }
    }
}

/// Region at `addr`
fn region(model: MacModel, overlay: bool, addr: Address) -> Region {
    if model >= MacModel::SE {
        region_se(model, overlay, addr)
    } else {
        region_plus(model, overlay, addr)
    }
}

/// 128K, 512K, 512Ke and Plus: the devices are selected from A23-A19 only
/// (BMU1 PAL) and appear all over their block
fn region_plus(model: MacModel, overlay: bool, addr: Address) -> Region {
    match addr {
        0x0000_0000..=0x003F_FFFF if !overlay => Region::Ram,
        // ROM in place of the RAM during the boot overlay
        0x0000_0000..=0x000F_FFFF | 0x0020_0000..=0x002F_FFFF => Region::Rom,
        0x0040_0000..=0x0043_FFFF => Region::Rom,
        // Plus with SCSI has no repeated ROM images above 0x440000 as
        // indication of SCSI controller present.
        //
        // 512Ke (using Plus ROM) does have repeated ROM images
        0x0044_0000..=0x004F_FFFF if model == MacModel::Plus && !overlay => Region::OpenBus,
        0x0044_0000..=0x004F_FFFF => Region::Rom,
        0x0058_0000..=0x005F_FFFF if model.has_scsi() => Region::Scsi,
        0x0060_0000..=0x006F_FFFF if !overlay => Region::Ram,
        0x0060_0000..=0x007F_FFFF if overlay => Region::RamOverlay,
        // Read at $800000-$9FFFFF, write at $A00000-$BFFFFF
        0x0080_0000..=0x00BF_FFFF => Region::Scc,
        0x00C0_0000..=0x00DF_FFFF => Region::Iwm,
        0x00E8_0000..=0x00EF_FFFF => Region::Via,
        // The VIA (A20=0) and the phase read (A19=0) are both selected at
        // $E00000-$E7FFFF, which Apple says not to use: give it to the
        // phase read
        0x00E0_0000..=0x00E7_FFFF | 0x00F0_0000..=0x00F7_FFFF => Region::PhaseRead,
        0x00F8_0000..=0x00F9_FFFF => Region::ExtRom,
        _ => Region::None,
    }
}

/// SE and Classic
fn region_se(model: MacModel, overlay: bool, addr: Address) -> Region {
    match addr {
        0x0000_0000..=0x003F_FFFF if !overlay => Region::Ram,
        // ROM in place of the RAM during the boot overlay
        0x0000_0000..=0x000F_FFFF | 0x0020_0000..=0x002F_FFFF => Region::Rom,
        0x0040_0000..=0x004F_FFFF => Region::Rom,
        0x0058_0000..=0x005F_FFFF if model.has_scsi() => Region::Scsi,
        0x0060_0000..=0x006F_FFFF if !overlay => Region::Ram,
        0x0060_0000..=0x007F_FFFF if overlay => Region::RamOverlay,
        0x009E_0000..=0x009F_FFFF | 0x00BE_0000..=0x00BF_FFFF => Region::Scc,
        0x00DE_0000..=0x00DF_FFFF => Region::Iwm,
        0x00EE_0000..=0x00EF_FFFF => Region::Via,
        0x00F0_0000..=0x00F7_FFFF if overlay => Region::PhaseRead,
        0x00F8_0000..=0x00F9_FFFF => Region::ExtRom,
        _ => Region::None,
    }
}

fn build_map(model: MacModel) -> AddressMap {
    std::array::from_fn(|overlay| {
        std::array::from_fn(|block| {
            region(model, overlay == 1, (block as Address) << MAP_BLOCK_SHIFT)
        })
    })
}

fn empty_map() -> AddressMap {
    [[Region::None; MAP_BLOCKS]; 2]
}

impl<TRenderer> Debuggable for CompactMacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_nest;
        use crate::debuggable::*;

        let mut result = vec![
            dbgprop_nest!("SWIM", self.swim),
            dbgprop_nest!("VIA (SY6522)", self.via),
            dbgprop_nest!("Video circuit", self.video),
        ];

        if self.model.has_scsi() {
            result.push(dbgprop_nest!("SCSI controller (NCR 5380)", self.scsi));
        }

        if self.model.has_adb() {
            result.push(dbgprop_nest!("Apple Desktop Bus", self.via.adb));
        }

        result
    }
}

/// A byte for every byte of a `len` bytes transfer
fn both_bytes(val: Byte, len: u32) -> u32 {
    if len == 2 {
        u32::from(val) * 0x0101
    } else {
        u32::from(val)
    }
}
