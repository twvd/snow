use std::fmt::Display;

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::mac::MacMonitor;
use crate::renderer::{DisplayBuffer, Renderer};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Field32, LatchingEvent, Word};

bitfield! {
    /// Control register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CtrlReg(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub low: u8 @ 0..=7,
        pub high: u8 @ 8..=15,

        pub reset: bool @ 15,
        pub pixelclock: u8 @ 12..=14,

        pub sense_out: u8 @ 9..=11,
        pub sense_in2: bool @ 9,
        pub sense_in1: bool @ 10,
        pub sense_in0: bool @ 11,

        pub transfer: bool @ 6,
        pub convolution: bool @ 5,
        pub interlace: bool @ 4,
        pub refresh: bool @ 3,
        pub rgb: bool @ 2,
        pub ram: bool @ 0,
    }
}

bitfield! {
    /// RAMDAC control register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RamdacCtrlReg(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub mode: u8 @ 1..=5,
        pub conv: bool @ 0,
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, strum::IntoStaticStr, Serialize, Deserialize)]
pub enum Bpp {
    /// 1bpp (black & white)
    One,

    /// 2bpp paletted (4 colors)
    Two,

    /// 4bpp paletted (16 colors)
    Four,

    /// 8bpp paletted (256 colors)
    Eight,

    /// 24-bit direct color ('Millions' of colors)
    TwentyFour,
}

/// Macintosh Display Card 1.2.341-0868
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Mdc12<TRenderer: Renderer> {
    #[serde(skip)]
    pub renderer: Option<TRenderer>,

    monitor: MacMonitor,
    rom: Vec<u8>,
    ctrl: CtrlReg,
    ramdac_ctrl: RamdacCtrlReg,
    vblank_irq: bool,
    vblank_enable: bool,
    vblank_ticks: Ticks,
    pub vram: Vec<u8>,
    toggle: bool,
    pub render: LatchingEvent,
    base: Field32,
    stride: Field32,
    pub palette: Vec<u32>,
    palette_addr: Field32,
    palette_wr: Field32,
    palette_cnt: usize,
}

impl<TRenderer> Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    pub fn new(rom: &[u8], renderer: TRenderer, monitor: MacMonitor) -> Self {
        Self {
            renderer: Some(renderer),
            monitor,
            rom: rom.to_owned(),
            ctrl: CtrlReg(0),
            ramdac_ctrl: RamdacCtrlReg(0),
            vblank_irq: false,
            vblank_enable: false,
            vram: vec![0; 0x1FFFFF],
            toggle: false,
            vblank_ticks: 0,
            palette: vec![0; 256],
            palette_addr: Field32(0),
            palette_wr: Field32(0),
            render: LatchingEvent::default(),
            base: Field32(0),
            stride: Field32(0),
            palette_cnt: 0,
        }
    }

    pub fn reset(&mut self) {
        self.vblank_irq = false;
        self.vblank_enable = false;
        self.ctrl.0 = 0;
    }

    pub fn get_irq(&self) -> bool {
        self.vblank_irq
    }

    fn read_ctrl(&self) -> CtrlReg {
        let msense = self.monitor.sense();
        let mut sense = msense[0];
        if self.ctrl.sense_in0() {
            sense &= msense[1];
        }
        if self.ctrl.sense_in1() {
            sense &= msense[2];
        }
        if self.ctrl.sense_in2() {
            sense &= msense[3];
        }

        self.ctrl.with_sense_out(sense)
    }

    pub fn bpp(&self) -> Bpp {
        match self.ramdac_ctrl.mode() {
            0 => Bpp::One,
            0x04 => Bpp::Two,
            0x08 => Bpp::Four,
            0x0C => Bpp::Eight,
            0x0D => Bpp::TwentyFour,
            _ => panic!("Unknown RAMDAC mode {:02X}", self.ramdac_ctrl.mode()),
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        let base_offset = match self.bpp() {
            // ??? not sure why this is off by 2 scanlines
            Bpp::TwentyFour => ((self.base.0 as usize) * 64 * 4) - (self.monitor.width() * 8),

            _ => (self.base.0 as usize) * 32,
        };
        &self.vram[base_offset..]
    }

    pub fn stride(&self) -> usize {
        let shift = if self.bpp() == Bpp::TwentyFour { 3 } else { 2 };
        (self.stride.0 as usize) << shift
    }

    /// Renders current dislayed frame to target DisplayBuffer
    pub fn render_to(&self, buf: &mut DisplayBuffer) {
        let fb = self.framebuffer();
        let palette = &self.palette;
        buf.set_size(self.monitor.width(), self.monitor.height());
        match self.bpp() {
            Bpp::One => {
                for idx in 0..(self.monitor.width() * self.monitor.height()) {
                    let byte = idx / 8;
                    let bit = idx % 8;
                    if fb[byte] & (1 << (7 - bit)) == 0 {
                        buf[idx * 4] = 0xEE;
                        buf[idx * 4 + 1] = 0xEE;
                        buf[idx * 4 + 2] = 0xEE;
                    } else {
                        buf[idx * 4] = 0x22;
                        buf[idx * 4 + 1] = 0x22;
                        buf[idx * 4 + 2] = 0x22;
                    }
                    buf[idx * 4 + 3] = 0xFF;
                }
            }
            Bpp::Two => {
                for idx in 0..(self.monitor.width() * self.monitor.height()) {
                    let byte = idx / 4;
                    let shift = 6 - (idx % 4) * 2;
                    let color = palette[usize::from(fb[byte] >> shift) & 0x03];

                    buf[idx * 4] = color as u8;
                    buf[idx * 4 + 1] = (color >> 8) as u8;
                    buf[idx * 4 + 2] = (color >> 16) as u8;
                    buf[idx * 4 + 3] = 0xFF;
                }
            }
            Bpp::Four => {
                for idx in 0..(self.monitor.width() * self.monitor.height()) {
                    let byte = idx / 2;
                    let shift = 4 - (idx % 2) * 4;
                    let color = palette[usize::from(fb[byte] >> shift) & 0x0F];

                    buf[idx * 4] = color as u8;
                    buf[idx * 4 + 1] = (color >> 8) as u8;
                    buf[idx * 4 + 2] = (color >> 16) as u8;
                    buf[idx * 4 + 3] = 0xFF;
                }
            }
            Bpp::Eight => {
                for idx in 0..(self.monitor.width() * self.monitor.height()) {
                    let byte = fb[idx];
                    let color = palette[byte as usize];

                    buf[idx * 4] = color as u8;
                    buf[idx * 4 + 1] = (color >> 8) as u8;
                    buf[idx * 4 + 2] = (color >> 16) as u8;
                    buf[idx * 4 + 3] = 0xFF;
                }
            }
            Bpp::TwentyFour => {
                for idx in 0..(self.monitor.width() * self.monitor.height()) {
                    buf[idx * 4] = fb[idx * 4 + 1];
                    buf[idx * 4 + 1] = fb[idx * 4 + 2];
                    buf[idx * 4 + 2] = fb[idx * 4 + 3];
                    buf[idx * 4 + 3] = 0xFF;
                }
            }
        }
    }

    pub fn blank(&mut self) -> Result<()> {
        self.vram.fill(0xFF);
        self.ctrl.0 = 0;
        self.render()?;
        Ok(())
    }

    pub fn render(&mut self) -> Result<()> {
        // We have to move the renderer so we don't upset the borrow checker.
        let mut renderer = self.renderer.take().unwrap();
        self.render_to(renderer.buffer_mut());
        self.renderer = Some(renderer);
        self.renderer.as_mut().unwrap().update()?;
        Ok(())
    }
}

impl<TRenderer> BusMember<Address> for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn read(&mut self, addr: Address) -> Option<u8> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x00_0000..=0x1F_FFFF => Some(self.vram[addr as usize]),
            0x20_0002 => Some(self.read_ctrl().high()),
            0x20_0003 => Some(self.read_ctrl().low()),
            0x20_0008 => Some(self.base.be0()),
            0x20_0009 => Some(self.base.be1()),
            0x20_000A => Some(self.base.be2()),
            0x20_000B => Some(self.base.be3()),
            0x20_000C => Some(self.stride.be0()),
            0x20_000D => Some(self.stride.be1()),
            0x20_000E => Some(self.stride.be2()),
            0x20_000F => Some(self.stride.be3()),

            // CRTC beam position
            0x20_01C0..=0x20_01C3 => {
                if addr == 0x20_01C3 {
                    self.toggle = !self.toggle;
                }
                if self.toggle {
                    Some(0)
                } else {
                    Some(4)
                }
            }
            // This has to read 0
            0x20_01C4..=0x20_01CF => Some(0),

            // RAMDAC
            0x20_0200 => Some(self.palette_addr.be0()),
            0x20_0201 => Some(self.palette_addr.be1()),
            0x20_0202 => Some(self.palette_addr.be2()),
            0x20_0203 => Some(self.palette_addr.be3()),
            0x20_020B => Some(self.ramdac_ctrl.0),

            // ROM (byte lane 3)
            0xFE_0000..=0xFF_FFFF if addr % 4 == 3 => {
                Some(self.rom[((addr - 0xFE_0000) / 4) as usize])
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x00_0000..=0x1F_FFFF => {
                self.vram[addr as usize] = val;
                Some(())
            }
            0x20_0002 => {
                self.ctrl.set_high(val);
                self.ctrl.set_reset(false);
                Some(())
            }
            0x20_0003 => {
                self.ctrl.set_low(val);
                self.ctrl.set_reset(false);
                Some(())
            }
            // Screen base address
            0x20_0008 => Some(self.base.set_be0(val)),
            0x20_0009 => Some(self.base.set_be1(val)),
            0x20_000A => Some(self.base.set_be2(val)),
            0x20_000B => Some(self.base.set_be3(val)),

            // Scanline width
            0x20_000C => Some(self.stride.set_be0(val)),
            0x20_000D => Some(self.stride.set_be1(val)),
            0x20_000E => Some(self.stride.set_be2(val)),
            0x20_000F => Some(self.stride.set_be3(val)),

            // CRTC
            0x20_013C => {
                self.vblank_enable = val & (1 << 1) == 0;
                Some(())
            }
            // IRQ clear
            0x20_0148 => {
                self.vblank_irq = false;
                Some(())
            }
            0x20_0149..=0x20_014B => Some(()),
            // RAMDAC
            0x20_0200 => Some(self.palette_addr.set_be0(val)),
            0x20_0201 => Some(self.palette_addr.set_be1(val)),
            0x20_0202 => Some(self.palette_addr.set_be2(val)),
            0x20_0203 => Some(self.palette_addr.set_be3(val)),

            // Palette memory. Written in full word/long but only the bottom byte is relevant
            0x20_0204..=0x20_0206 => Some(()),
            0x20_0207 => {
                self.palette_wr.0 >>= 8;
                self.palette_wr.0 |= (val as u32) << 24;
                self.palette_cnt += 1;
                if self.palette_cnt == 3 {
                    self.palette[(self.palette_addr.0 % 256) as usize] = self.palette_wr.0 >> 8;
                    self.palette_wr.0 = 0;
                    self.palette_addr.0 += 1;
                    self.palette_cnt = 0;
                }
                Some(())
            }
            0x20_020B => Some(self.ramdac_ctrl.0 = val),
            _ => None,
        }
    }
}

impl<TRenderer> Tickable for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks > 16_000_000 / 60 {
            self.render()?;

            self.vblank_ticks = 0;
            if self.vblank_enable {
                self.vblank_irq = true;
            }
        }
        Ok(ticks)
    }
}

impl<TRenderer> Debuggable for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_group};
        use crate::{dbgprop_byte_bin, dbgprop_enum, dbgprop_long, dbgprop_word_bin};

        vec![
            dbgprop_group!(
                "Registers",
                vec![
                    dbgprop_word_bin!("Control", self.ctrl.0),
                    dbgprop_long!("Screen base", self.base.0),
                    dbgprop_long!("Screen stride", self.stride.0),
                    dbgprop_byte_bin!("RAMDAC control", self.ramdac_ctrl.0),
                    dbgprop_long!("Palette write index", self.palette_addr.0)
                ]
            ),
            dbgprop_enum!("Monitor", self.monitor),
            dbgprop_enum!("BPP", self.bpp()),
            dbgprop_bool!("VBlank enable", self.vblank_enable),
            dbgprop_bool!("VBlank IRQ", self.vblank_irq),
        ]
    }
}

impl<TRenderer> Display for Mdc12<TRenderer>
where
    TRenderer: Renderer,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Macintosh Display Card 8-24")
    }
}
