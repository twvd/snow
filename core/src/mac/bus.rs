use std::ops::Range;

use super::audio::{AudioReceiver, AudioState};
use super::scc::Scc;
use super::scsi::ScsiController;
use super::via::Via;
use super::MacModel;
use crate::bus::{Address, Bus, BusMember, BusResult, InspectableBus, IrqSource};
use crate::emulator::comm::EmulatorSpeed;
use crate::mac::iwm::Iwm;
use crate::mac::video::Video;
use crate::renderer::Renderer;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent};

use anyhow::Result;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};

pub struct MacBus<TRenderer: Renderer> {
    cycles: Ticks,

    /// The currently emulated Macintosh model
    model: MacModel,

    /// Trace non-ROM/RAM access
    pub trace: bool,

    rom: Vec<u8>,
    pub(crate) ram: Vec<u8>,
    pub(crate) via: Via,
    scc: Scc,
    pub(crate) video: Video<TRenderer>,
    pub(crate) audio: AudioState,
    eclock: Ticks,
    mouse_ready: bool,
    pub(crate) iwm: Iwm,
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

    pub dbg_break: LatchingEvent,

    overlay: bool,

    /// Toggles ROM mirroring for SCSI support
    scsi_enable: bool,

    /// Emulation speed setting
    pub(crate) speed: EmulatorSpeed,

    /// Last pushed audio sample
    last_audiosample: u8,
}

impl<TRenderer> MacBus<TRenderer>
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

    pub fn new(model: MacModel, rom: &[u8], renderer: TRenderer) -> Self {
        let ram_size = model.ram_size();
        let fb_alt_start = ram_size as Address - Video::<TRenderer>::FRAMEBUFFER_ALT_OFFSET;
        let fb_main_start = ram_size as Address - Video::<TRenderer>::FRAMEBUFFER_MAIN_OFFSET;
        let sound_alt_start = ram_size - Self::SOUND_ALT_OFFSET;
        let sound_main_start = ram_size - Self::SOUND_MAIN_OFFSET;

        Self {
            cycles: 0,
            model,
            trace: false,

            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            via: Via::new(model),
            video: Video::new(renderer),
            audio: AudioState::default(),
            eclock: 0,
            scc: Scc::new(),
            iwm: Iwm::new(model.fdd_double_sided(), model.fdd_count()),
            scsi: ScsiController::new(),
            mouse_ready: false,

            ram_mask: (ram_size - 1),
            rom_mask: rom.len() - 1,

            fb_main: fb_main_start
                ..(fb_main_start + Video::<TRenderer>::FRAMEBUFFER_SIZE as Address),
            fb_alt: fb_alt_start..(fb_alt_start + Video::<TRenderer>::FRAMEBUFFER_SIZE as Address),

            soundbuf_main: sound_main_start..(sound_main_start + Self::SOUNDBUF_SIZE),
            soundbuf_alt: sound_alt_start..(sound_alt_start + Self::SOUNDBUF_SIZE),

            dbg_break: LatchingEvent::default(),
            overlay: true,
            scsi_enable: true,
            speed: EmulatorSpeed::Accurate,
            last_audiosample: 0,
        }
    }

    pub(crate) fn get_audio_channel(&self) -> AudioReceiver {
        self.audio.receiver.clone()
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
        if self.trace && !(0x0060_0000..=0x007F_FFFF).contains(&addr) {
            trace!("WRO {:08X} - {:02X}", addr, val);
        }

        match addr {
            // ROM (disables overlay)
            0x0040_0000..=0x0057_FFFF if self.model >= MacModel::SE => {
                self.overlay = false;
                self.write_normal(addr, val)
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF => self.scsi.write(addr, val),
            // RAM
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & self.ram_mask] = val),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.write(addr, val),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.write(addr, val),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.write(addr, val),
            _ => None,
        }
    }

    fn write_normal(&mut self, addr: Address, val: Byte) -> Option<()> {
        if self.trace && !(0x0000_0000..=0x003F_FFFF).contains(&addr) {
            trace!("WR {:08X} - {:02X}", addr, val);
        }

        // Duplicate framebuffers to video component
        // (writes also go through RAM)
        if self.fb_main.contains(&addr) {
            let offset = (addr - self.fb_main.start) as usize;
            self.video.framebuffers[0][offset] = val;
        }
        if self.fb_alt.contains(&addr) {
            let offset = (addr - self.fb_alt.start) as usize;
            self.video.framebuffers[1][offset] = val;
        }

        match addr {
            // RAM
            0x0000_0000..=0x003F_FFFF => Some(self.ram[addr as usize & self.ram_mask] = val),
            // SCSI
            0x0058_0000..=0x005F_FFFF => self.scsi.write(addr, val),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.write(addr, val),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.write(addr, val),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => {
                self.via.write(addr, val);

                Some(())
            }
            _ => None,
        }
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            // ROM
            0x0000_0000..=0x000F_FFFF | 0x0020_0000..=0x002F_FFFF | 0x0040_0000..=0x0043_FFFF => {
                Some(self.rom[addr as usize & self.rom_mask])
            }
            0x0044_0000..=0x004F_FFFF if !self.scsi_enable => {
                Some(self.rom[addr as usize & self.rom_mask])
            }
            // Overlay flip for Mac SE+
            0x0040_0000..=0x005F_FFFF if self.model >= MacModel::SE => {
                self.overlay = false;
                self.read_normal(addr)
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF => self.scsi.read(addr),
            // RAM
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            // Phase adjust (ignore)
            0x009F_FFF7 | 0x009F_FFF9 => Some(0xFF),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.read(addr),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.read(addr),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),
            // Phase read (ignore)
            0x00F0_0000..=0x00F7_FFFF => Some(0xFF),
            // Test software region (ignore)
            0x00F8_0000..=0x00F9_FFFF => Some(0xFF),

            _ => None,
        };
        if self.trace && !(0x0000_0000..=0x007F_FFFF).contains(&addr) {
            trace!("RDO {:08X} - {:02X?}", addr, result);
        }

        result
    }

    fn read_normal(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            // RAM
            0x0000_0000..=0x003F_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            // ROM
            0x0040_0000..=0x0043_FFFF => Some(self.rom[addr as usize & self.rom_mask]),
            0x0044_0000..=0x004F_FFFF if !self.scsi_enable => {
                Some(self.rom[addr as usize & self.rom_mask])
            }
            // SCSI
            0x0058_0000..=0x005F_FFFF => self.scsi.read(addr),
            // SCC
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.read(addr),
            // IWM
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.read(addr),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),
            // Test software region (ignore)
            0x00F8_0000..=0x00F9_FFFF => Some(0xFF),

            _ => None,
        };

        if self.trace && !(0x0000_0000..=0x004F_FFFF).contains(&addr) {
            trace!("RD {:08X} - {:02X?}", addr, result);
        }
        result
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update_rel(&mut self, relx: i16, rely: i16, button: Option<bool>) {
        let old_x = self.read_ram::<u16>(Self::ADDR_RAWMOUSE_X);
        let old_y = self.read_ram::<u16>(Self::ADDR_RAWMOUSE_Y);

        if !self.mouse_ready && (old_x != 15 || old_y != 15) {
            // Wait until the boot process has initialized the mouse position so we don't
            // interfere with the memory test.
            return;
        }
        self.mouse_ready = true;

        if relx != 0 || rely != 0 {
            let new_x = old_x.wrapping_add_signed(relx);
            let new_y = old_y.wrapping_add_signed(rely);

            // Report updated mouse coordinates to OS
            self.write_ram(Self::ADDR_MTEMP_X, new_x);
            self.write_ram(Self::ADDR_MTEMP_Y, new_y);
            if self.model >= MacModel::SE {
                // SE+ needs to see even a small difference between the current (RawMouse)
                // and new (MTemp) position, otherwise the change is ignored.
                self.write_ram(Self::ADDR_RAWMOUSE_X, new_x - 1);
                self.write_ram(Self::ADDR_RAWMOUSE_Y, new_y + 1);
            } else {
                self.write_ram(Self::ADDR_RAWMOUSE_X, new_x);
                self.write_ram(Self::ADDR_RAWMOUSE_Y, new_y);
            }
            self.write_ram(Self::ADDR_CRSRNEW, 1_u8);
        }

        if let Some(b) = button {
            if self.model.has_adb() {
                // TODO ADB
            } else {
                // Mouse button through VIA I/O
                self.via.b_in.set_sw(!b);
            }
        }
    }

    /// Updates the mouse position (absolute coordinates)
    pub fn mouse_update_abs(&mut self, x: u16, y: u16) {
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
            self.write_ram(Self::ADDR_RAWMOUSE_X, x - 1);
            self.write_ram(Self::ADDR_RAWMOUSE_Y, y + 1);
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
}

impl<TRenderer> Bus<Address, Byte> for MacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    fn read(&mut self, addr: Address) -> BusResult<Byte> {
        if (0x0000_0000..=0x003F_FFFF).contains(&addr)
            && !self.video.in_blanking_period()
            && !self.model.ram_interleave_cpu(self.cycles)
        {
            // RAM access for CPU currently blocked by memory controller
            // https://www.bigmessowires.com/2011/08/25/68000-interleaved-memory-controller-design/
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
            BusResult::Ok(0xFF)
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> BusResult<Byte> {
        if (0x0000_0000..=0x003F_FFFF).contains(&addr)
            && !self.video.in_blanking_period()
            && !self.model.ram_interleave_cpu(self.cycles)
        {
            // RAM access for CPU currently blocked by memory controller
            // https://www.bigmessowires.com/2011/08/25/68000-interleaved-memory-controller-design/
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
        self.iwm.sel = self.via.a_out.sel();
        self.video.framebuffer_select = self.via.a_out.page2();

        if written.is_none() {
            warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
        }
        BusResult::Ok(val)
    }
}

impl<TRenderer> Tickable for MacBus<TRenderer>
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
            self.iwm.intdrive = self.via.a_out.drivesel();
        }

        // VBlank interrupt
        if self.video.get_clr_vblank() {
            self.via.ifr.set_vblank(true);
        }

        // HBlank
        if self.video.get_clr_hblank() {
            // Update floppy drive PWM and send next audio sample
            let scanline = self.video.get_scanline();
            let soundon = self.via.a_out.sound() > 0 && !self.via.b_out.sndenb();
            let soundbuf = self.soundbuf();
            let pwm = soundbuf[scanline * 2 + 1];
            let audiosample = if soundon { soundbuf[scanline * 2] } else { 0 };

            self.iwm.push_pwm(pwm)?;

            // Emulator will block here to sync to audio frequency
            match self.speed {
                EmulatorSpeed::Accurate => self.audio.push(audiosample)?,
                EmulatorSpeed::Dynamic => {
                    if !self.audio.is_silent() || audiosample != self.last_audiosample {
                        self.audio.push(audiosample)?;
                    }
                }
                EmulatorSpeed::Uncapped => (),
            }
            self.last_audiosample = audiosample;
        }

        self.iwm.tick(1)?;

        Ok(1)
    }
}

impl<TRenderer> IrqSource for MacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_irq(&mut self) -> Option<u8> {
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

impl<TRenderer> InspectableBus<Address, Byte> for MacBus<TRenderer>
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
