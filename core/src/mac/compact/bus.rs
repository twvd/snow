use std::ops::Range;
use std::thread;
use std::time::{Duration, Instant};

use super::audio::AudioState;
use super::video::Video;
use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::debuggable::Debuggable;
use crate::emulator::comm::EmulatorSpeed;
use crate::emulator::MouseMode;
use crate::keymap::KeyEvent;
use crate::mac::adb::{AdbEvent, AdbKeyboard, AdbMouse};
use crate::mac::rtc::Rtc;
use crate::mac::scc::{Scc, SccCh};
use crate::mac::scsi::controller::ScsiController;
use crate::mac::swim::drive::DriveType;
use crate::mac::swim::Swim;
use crate::mac::via::Via;
use crate::mac::MacModel;
use crate::renderer::{AudioReceiver, Renderer};
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

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Last pushed audio sample
    last_audiosample: u8,

    /// Last vblank time (for syncing to video)
    /// Not serializing this because it is only used for determining how long to
    /// sleep for in Video speed mode.
    #[serde(skip, default = "Instant::now")]
    vblank_time: Instant,

    /// VPA/E-clock sync in progress
    vpa_sync: bool,

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
            speed: EmulatorSpeed::Accurate,
            last_audiosample: 0,
            vblank_time: Instant::now(),
            vpa_sync: false,
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

    pub(crate) fn get_audio_channel(&self) -> AudioReceiver {
        self.audio.receiver.as_ref().unwrap().clone()
    }

    fn soundbuf(&mut self) -> &mut [u8] {
        if self.model >= MacModel::SE || self.via.a_out.sndpg2() {
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

    fn write_overlay(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // ROM (disables overlay)
            0x0040_0000..=0x0057_FFFF if self.model >= MacModel::SE => {
                self.overlay = false;
                self.write_normal(addr, val)
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF if self.model.has_scsi() => self.scsi.write(addr, val),
            // RAM
            0x0060_0000..=0x007F_FFFF => {
                let idx = ((addr as usize) - 0x60_0000) & self.ram_mask;
                self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                Some(self.ram[idx] = val)
            }
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.write(addr >> 1, val),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.swim.write(addr, val),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.write(addr, val),
            _ => None,
        }
    }

    fn write_normal(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr {
            // RAM
            0x0000_0000..=0x003F_FFFF | 0x0060_0000..=0x006F_FFFF => {
                // Duplicate framebuffers to video component
                // (writes also go through RAM)
                if self.fb_main.contains(&(addr & self.ram_mask as Address)) {
                    let offset = ((addr & self.ram_mask as Address) - self.fb_main.start) as usize;
                    self.video.framebuffers[0][offset] = val;
                }
                if self.fb_alt.contains(&(addr & self.ram_mask as Address)) {
                    let offset = ((addr & self.ram_mask as Address) - self.fb_alt.start) as usize;
                    self.video.framebuffers[1][offset] = val;
                }

                let idx = addr as usize & self.ram_mask;
                self.ram_dirty.insert(idx / RAM_DIRTY_PAGESIZE);
                Some(self.ram[idx] = val)
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF if self.model.has_scsi() => self.scsi.write(addr, val),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.write(addr >> 1, val),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.swim.write(addr, val),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => {
                self.via.write(addr, val);

                Some(())
            }
            _ => None,
        }
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // Overlay flip for Mac SE+
            0x0040_0000..=0x004F_FFFF if self.model >= MacModel::SE => {
                self.overlay = false;
                self.read_normal(addr)
            }
            // ROM
            0x0000_0000..=0x000F_FFFF | 0x0020_0000..=0x002F_FFFF | 0x0040_0000..=0x004F_FFFF => {
                Some(
                    *self
                        .rom
                        .get(addr as usize & self.rom_mask)
                        .unwrap_or(&self.openbus[(addr & 1) as usize]),
                )
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF if self.model.has_scsi() => self.scsi.read(addr),
            // RAM
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            // Phase adjust (ignore)
            0x009F_FFF7 | 0x009F_FFF9 => Some(0),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.read(addr >> 1),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.swim.read(addr),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),
            // Phase read (ignore)
            0x00F0_0000..=0x00F7_FFFF => Some(0),
            // Test software region / extension ROM
            0x00F8_0000..=0x00F9_FFFF => Some(
                *self
                    .extension_rom
                    .get((addr - 0xF8_0000) as usize)
                    .unwrap_or(&self.openbus[(addr & 1) as usize]),
            ),

            _ => None,
        }
    }

    fn read_normal(&mut self, addr: Address) -> Option<Byte> {
        match addr {
            // RAM
            0x0000_0000..=0x003F_FFFF | 0x0060_0000..=0x006F_FFFF => {
                Some(self.ram[addr as usize & self.ram_mask])
            }
            // ROM
            0x0040_0000..=0x0043_FFFF => Some(
                *self
                    .rom
                    .get(addr as usize & self.rom_mask)
                    .unwrap_or(&self.openbus[(addr & 1) as usize]),
            ),
            0x0044_0000..=0x004F_FFFF => {
                if self.model == MacModel::Plus {
                    // Plus with SCSI has no repeated ROM images above 0x440000 as
                    // indication of SCSI controller present.
                    //
                    // 512Ke (using Plus ROM) does have repeated ROM images
                    Some(self.openbus[(addr & 1) as usize])
                } else {
                    Some(
                        *self
                            .rom
                            .get(addr as usize & self.rom_mask)
                            .unwrap_or(&self.openbus[(addr & 1) as usize]),
                    )
                }
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF if self.model.has_scsi() => self.scsi.read(addr),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.read(addr >> 1),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.swim.read(addr),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),
            // Test software region / extension ROM
            0x00F8_0000..=0x00F9_FFFF => Some(
                *self
                    .extension_rom
                    .get((addr - 0xF8_0000) as usize)
                    .unwrap_or(&self.openbus[(addr & 1) as usize]),
            ),

            _ => None,
        }
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
    fn in_waitstate(&mut self, addr: Address) -> bool {
        // DTACK (only for RAM region)
        if (0x0000_0000..=0x003F_FFFF).contains(&addr)
            && !self.video.in_blanking_period()
            && !self.model.ram_interleave_cpu(self.cycles)
        {
            // RAM access for CPU currently blocked by memory controller
            // https://www.bigmessowires.com/2011/08/25/68000-interleaved-memory-controller-design/
            return true;
        }

        // VPA
        if addr >= 0xE0_0000 {
            if !self.vpa_sync {
                // Start E-Clock synchronization, wait for next low edge.
                self.vpa_sync = true;
            } else if self.eclock == 0 {
                // Low edge, synchronized
                self.vpa_sync = false;
                return false;
            }
            return true;
        }

        false
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
            self.openbus[(addr & 1) as usize] = v;
            BusResult::Ok(v)
        } else {
            warn!("Read from unimplemented address: {:08X}", addr);
            BusResult::Ok(self.openbus[(addr & 1) as usize])
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

        if self.overlay && self.model <= MacModel::Plus && !self.via.a_out.overlay() {
            self.overlay = false;
        }

        // Sync values that live in multiple places
        self.swim.sel = self.via.a_out.sel();
        self.video.framebuffer_select = self.via.a_out.page2();

        if written.is_none() {
            warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
        }

        self.openbus[(addr & 1) as usize] = val;
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

            self.overlay = true;
        }

        // Keep the RTC and ADB for PRAM and event channels
        let Via { adb, rtc, .. } = std::mem::replace(&mut self.via, Via::new(self.model));
        self.via.adb = adb;
        self.via.rtc = rtc;

        self.scc = Scc::new();
        Ok(())
    }
}

impl<TRenderer> Tickable for CompactMacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // This is called from the CPU, at the CPU clock speed
        assert_eq!(ticks, 1);
        self.cycles += ticks;

        self.eclock += ticks;
        while self.eclock >= 10 {
            // The E Clock is roughly 1/10th of the CPU clock
            // TODO ticks when VPA is asserted
            self.eclock -= 10;

            self.via.tick(1)?;
        }

        // Pixel clock (15.6672 MHz) is roughly 2x CPU speed
        self.video.tick(2)?;

        // Sync VIA registers
        if self.model <= MacModel::Plus {
            self.via.b_in.set_h4(self.video.in_hblank());
        } else {
            self.swim.intdrive = self.via.a_out.drivesel();
        }

        // VBlank interrupt
        if self.video.get_clr_vblank() {
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

        // HBlank
        if self.video.get_clr_hblank() {
            // Update floppy drive PWM and send next audio sample
            let scanline = self.video.get_scanline();
            let soundon =
                self.via.a_out.sound() > 0 && self.via.ddrb.sndenb() && !self.via.b_out.sndenb();
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
            }
            self.last_audiosample = audiosample;
        }

        self.swim.tick(1)?;

        Ok(1)
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
        if self.via.ifr.0 & self.via.ier.0 != 0 {
            return Some(1);
        }
        // SCSI IRQs
        if self.model >= MacModel::SE && self.scsi.get_irq() && !self.via.b_out.scsi_int() {
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
            None
        } else if self.overlay {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        // Everything up to 0x800000 is safe (RAM/ROM only)
        if addr >= 0x80_0000 {
            None
        } else if self.overlay {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        }
    }
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
