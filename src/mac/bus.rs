use std::ops::Range;

use super::scc::Scc;
use super::via::Via;
use crate::bus::{Address, Bus, BusMember, InspectableBus, IrqSource};
use crate::frontend::Renderer;
use crate::mac::iwm::Iwm;
use crate::mac::video::Video;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent};

use anyhow::Result;
use log::*;
use num_traits::{FromPrimitive, PrimInt, ToBytes};

pub struct MacBus<TRenderer: Renderer> {
    /// Trace non-ROM/RAM access
    pub trace: bool,

    rom: Vec<u8>,
    pub ram: Vec<u8>,
    pub via: Via,
    scc: Scc,
    video: Video<TRenderer>,
    eclock: Ticks,
    mouse_ready: bool,
    pub iwm: Iwm,

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

    pub fn new(rom: &[u8], ram_size: usize, renderer: TRenderer, fd_double: bool) -> Self {
        let fb_alt_start = ram_size as Address - Video::<TRenderer>::FRAMEBUFFER_ALT_OFFSET;
        let fb_main_start = ram_size as Address - Video::<TRenderer>::FRAMEBUFFER_MAIN_OFFSET;
        let sound_alt_start = ram_size - Self::SOUND_ALT_OFFSET;
        let sound_main_start = ram_size - Self::SOUND_MAIN_OFFSET;

        Self {
            trace: false,

            rom: Vec::from(rom),
            ram: vec![0; ram_size],
            via: Via::new(),
            video: Video::new(renderer),
            eclock: 0,
            scc: Scc::new(),
            iwm: Iwm::new(fd_double),
            mouse_ready: false,

            ram_mask: (ram_size - 1),
            rom_mask: rom.len() - 1,

            fb_main: fb_main_start
                ..(fb_main_start + Video::<TRenderer>::FRAMEBUFFER_SIZE as Address),
            fb_alt: fb_alt_start..(fb_alt_start + Video::<TRenderer>::FRAMEBUFFER_SIZE as Address),

            soundbuf_main: sound_main_start..(sound_main_start + Self::SOUNDBUF_SIZE),
            soundbuf_alt: sound_alt_start..(sound_alt_start + Self::SOUNDBUF_SIZE),

            dbg_break: LatchingEvent::default(),
        }
    }

    fn soundbuf(&mut self) -> &mut [u8] {
        if self.via.a_out.sndpg2() {
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
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & self.ram_mask] = val),
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.write(addr, val),
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
            0x0000_0000..=0x003F_FFFF => Some(self.ram[addr as usize & self.ram_mask] = val),
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.write(addr, val),
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.write(addr, val),
            // VIA
            0x00EF_0000..=0x00EF_FFFF => self.via.write(addr, val),
            _ => None,
        }
    }

    fn read_overlay(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            0x0000_0000..=0x000F_FFFF | 0x0020_0000..=0x002F_FFFF | 0x0040_0000..=0x004F_FFFF => {
                Some(self.rom[addr as usize & self.rom_mask])
            }
            0x0060_0000..=0x007F_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.read(addr),
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.read(addr),
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),

            _ => None,
        };
        if self.trace && !(0x0000_0000..=0x007F_FFFF).contains(&addr) {
            trace!("RDO {:08X} - {:02X?}", addr, result);
        }

        result
    }

    fn read_normal(&mut self, addr: Address) -> Option<Byte> {
        let result = match addr {
            0x0000_0000..=0x003F_FFFF => Some(self.ram[addr as usize & self.ram_mask]),
            0x0040_0000..=0x004F_FFFF => Some(self.rom[addr as usize & self.rom_mask]),
            0x009F_0000..=0x009F_FFFF | 0x00BF_0000..=0x00BF_FFFF => self.scc.read(addr),
            0x00DF_E1FF..=0x00DF_FFFF => self.iwm.read(addr),
            0x00EF_0000..=0x00EF_FFFF => self.via.read(addr),

            _ => None,
        };

        if self.trace && !(0x0000_0000..=0x004F_FFFF).contains(&addr) {
            trace!("RD {:08X} - {:02X?}", addr, result);
        }
        result
    }

    /// Updates the mouse position (relative coordinates) and button state
    pub fn mouse_update(&mut self, relx: i16, rely: i16, button: Option<bool>) {
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
            self.write_ram(Self::ADDR_RAWMOUSE_X, new_x);
            self.write_ram(Self::ADDR_RAWMOUSE_Y, new_y);
            self.write_ram(Self::ADDR_CRSRNEW, 1_u8);
        }

        // Mouse button through VIA I/O
        if let Some(b) = button {
            self.via.b_in.set_sw(!b);
        }
    }
}

impl<TRenderer> Bus<Address, Byte> for MacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_mask(&self) -> Address {
        0x00FFFFFF
    }

    fn read(&mut self, addr: Address) -> Byte {
        let val = if self.via.a_out.overlay() {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        };

        if let Some(v) = val {
            v
        } else {
            warn!("Read from unimplemented address: {:08X}", addr);
            0xFF
        }
    }

    fn write(&mut self, addr: Address, val: Byte) {
        let written = if self.via.a_out.overlay() {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        };

        // Sync values that live in multiple places
        self.iwm.sel = self.via.a_out.sel();
        self.video.framebuffer_select = self.via.a_out.page2();

        if written.is_none() {
            warn!("Write to unimplemented address: {:08X} {:02X}", addr, val);
        }
    }
}

impl<TRenderer> Tickable for MacBus<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // This is called from the CPU, at the CPU clock speed
        assert_eq!(ticks, 1);

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
        self.via.b_in.set_h4(self.video.in_hblank());

        // VBlank interrupt
        if self.video.get_clr_vblank() && self.via.ier.vblank() {
            self.via.ifr.set_vblank(true);
        }

        // HBlank
        if self.video.get_clr_hblank() {
            // Update floppy drive PWM
            let scanline = self.video.get_scanline();
            if scanline < (Self::SOUNDBUF_SIZE / 2) {
                let soundbuf = self.soundbuf();
                let pwm = soundbuf[scanline * 2 + 1];
                self.iwm.push_pwm(pwm)?;
            }
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
        } else if self.via.a_out.overlay() {
            self.read_overlay(addr)
        } else {
            self.read_normal(addr)
        }
    }

    fn inspect_write(&mut self, addr: Address, val: Byte) -> Option<()> {
        // Everything up to 0x800000 is safe (RAM/ROM only)
        if addr >= 0x80_0000 {
            None
        } else if self.via.a_out.overlay() {
            self.write_overlay(addr, val)
        } else {
            self.write_normal(addr, val)
        }
    }
}
