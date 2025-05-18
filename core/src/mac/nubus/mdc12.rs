use std::fmt::Display;

use anyhow::Result;
use proc_bitfield::bitfield;

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Field32, LatchingEvent, Word};

pub const RGB8_PALETTE: [u32; 256] = [
    0x000000, 0x0b0b0b, 0x222222, 0x444444, 0x555555, 0x777777, 0x888888, 0xaaaaaa, 0xbbbbbb,
    0xdddddd, 0xeeeeee, 0x00000b, 0x000022, 0x000044, 0x000055, 0x000077, 0x000088, 0x0000aa,
    0x0000bb, 0x0000dd, 0x0000ee, 0x000b00, 0x002200, 0x004400, 0x005500, 0x007700, 0x008800,
    0x00aa00, 0x00bb00, 0x00dd00, 0x00ee00, 0x0b0000, 0x220000, 0x440000, 0x550000, 0x770000,
    0x880000, 0xaa0000, 0xbb0000, 0xdd0000, 0xee0000, 0x000033, 0x000066, 0x000099, 0x0000cc,
    0x0000ff, 0x003300, 0x003333, 0x003366, 0x003399, 0x0033cc, 0x0033ff, 0x006600, 0x006633,
    0x006666, 0x006699, 0x0066cc, 0x0066ff, 0x009900, 0x009933, 0x009966, 0x009999, 0x0099cc,
    0x0099ff, 0x00cc00, 0x00cc33, 0x00cc66, 0x00cc99, 0x00cccc, 0x00ccff, 0x00ff00, 0x00ff33,
    0x00ff66, 0x00ff99, 0x00ffcc, 0x00ffff, 0x330000, 0x330033, 0x330066, 0x330099, 0x3300cc,
    0x3300ff, 0x333300, 0x333333, 0x333366, 0x333399, 0x3333cc, 0x3333ff, 0x336600, 0x336633,
    0x336666, 0x336699, 0x3366cc, 0x3366ff, 0x339900, 0x339933, 0x339966, 0x339999, 0x3399cc,
    0x3399ff, 0x33cc00, 0x33cc33, 0x33cc66, 0x33cc99, 0x33cccc, 0x33ccff, 0x33ff00, 0x33ff33,
    0x33ff66, 0x33ff99, 0x33ffcc, 0x33ffff, 0x660000, 0x660033, 0x660066, 0x660099, 0x6600cc,
    0x6600ff, 0x663300, 0x663333, 0x663366, 0x663399, 0x6633cc, 0x6633ff, 0x666600, 0x666633,
    0x666666, 0x666699, 0x6666cc, 0x6666ff, 0x669900, 0x669933, 0x669966, 0x669999, 0x6699cc,
    0x6699ff, 0x66cc00, 0x66cc33, 0x66cc66, 0x66cc99, 0x66cccc, 0x66ccff, 0x66ff00, 0x66ff33,
    0x66ff66, 0x66ff99, 0x66ffcc, 0x66ffff, 0x990000, 0x990033, 0x990066, 0x990099, 0x9900cc,
    0x9900ff, 0x993300, 0x993333, 0x993366, 0x993399, 0x9933cc, 0x9933ff, 0x996600, 0x996633,
    0x996666, 0x996699, 0x9966cc, 0x9966ff, 0x999900, 0x999933, 0x999966, 0x999999, 0x9999cc,
    0x9999ff, 0x99cc00, 0x99cc33, 0x99cc66, 0x99cc99, 0x99cccc, 0x99ccff, 0x99ff00, 0x99ff33,
    0x99ff66, 0x99ff99, 0x99ffcc, 0x99ffff, 0xcc0000, 0xcc0033, 0xcc0066, 0xcc0099, 0xcc00cc,
    0xcc00ff, 0xcc3300, 0xcc3333, 0xcc3366, 0xcc3399, 0xcc33cc, 0xcc33ff, 0xcc6600, 0xcc6633,
    0xcc6666, 0xcc6699, 0xcc66cc, 0xcc66ff, 0xcc9900, 0xcc9933, 0xcc9966, 0xcc9999, 0xcc99cc,
    0xcc99ff, 0xcccc00, 0xcccc33, 0xcccc66, 0xcccc99, 0xcccccc, 0xccccff, 0xccff00, 0xccff33,
    0xccff66, 0xccff99, 0xccffcc, 0xccffff, 0xff0000, 0xff0033, 0xff0066, 0xff0099, 0xff00cc,
    0xff00ff, 0xff3300, 0xff3333, 0xff3366, 0xff3399, 0xff33cc, 0xff33ff, 0xff6600, 0xff6633,
    0xff6666, 0xff6699, 0xff66cc, 0xff66ff, 0xff9900, 0xff9933, 0xff9966, 0xff9999, 0xff99cc,
    0xff99ff, 0xffcc00, 0xffcc33, 0xffcc66, 0xffcc99, 0xffcccc, 0xffccff, 0xffff00, 0xffff33,
    0xffff66, 0xffff99, 0xffffcc, 0xffffff,
];

bitfield! {
    /// Control register
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct CtrlReg(pub Word): Debug, FromRaw, IntoRaw, DerefRaw {
        pub low: u8 @ 0..=7,
        pub high: u8 @ 8..=15,

        pub reset: bool @ 15,
        pub pixelclock: u8 @ 12..=14,
        pub sense: u8 @ 9..=11,
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
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct RamdacCtrlReg(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        pub mode: u8 @ 1..=5,
        pub conv: bool @ 0,
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, strum::IntoStaticStr)]
pub enum Bpp {
    One,
    Two,
    Four,
    Eight,
    TwentyFour,
}

/// Macintosh Display Card 1.2.341-0868
pub struct Mdc12 {
    rom: Vec<u8>,
    ctrl: CtrlReg,
    ramdac_ctrl: RamdacCtrlReg,
    vblank_irq: bool,
    vblank_enable: bool,
    vblank_ticks: Ticks,
    pub vram: Vec<u8>,
    toggle: bool,
    clut_addr: [u8; 4],
    pub render: LatchingEvent,
    base: Field32,
    stride: Field32,
}

impl Mdc12 {
    pub fn new() -> Self {
        Self {
            rom: std::fs::read("341-0868.bin").expect("Graphics card ROM file"),
            ctrl: CtrlReg(0),
            ramdac_ctrl: RamdacCtrlReg(0),
            vblank_irq: false,
            vblank_enable: false,
            vram: vec![0; 0x1FFFFF],
            toggle: false,
            vblank_ticks: 0,
            clut_addr: [0; 4],
            render: LatchingEvent::default(),
            base: Field32(0),
            stride: Field32(0),
        }
    }

    pub fn get_irq(&self) -> bool {
        self.vblank_irq
    }

    fn read_ctrl(&self) -> CtrlReg {
        self.ctrl.with_sense(match self.ctrl.sense() {
            // RGB 12" monitor
            0 => 2,
            1 => 2,
            2 => 0,
            3 => 2,
            _ => unreachable!(),
        })
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
        let mut shift = 5;
        if self.bpp() == Bpp::TwentyFour {
            shift += 1;
        }
        if self.ctrl.convolution() {
            shift += 1;
        }
        let base_offset = (self.base.0 as usize) << shift;
        &self.vram[base_offset..]
    }
}

impl BusMember<Address> for Mdc12 {
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
            0x20_01C4..=0x20_01CF => Some(0),
            // RAMDAC
            0x20_0204..=0x20_0207 => Some(self.clut_addr[addr as usize - 0x200204]),
            0x20_020B => Some(self.ramdac_ctrl.0),
            0xFE_0000..=0xFF_FFFF if addr % 4 == 3 => {
                // ROM (byte lane 3)
                Some(self.rom[((addr - 0xFE_0000) / 4) as usize])
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: u8) -> Option<()> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x00_0000..=0x03_FFFF => {
                self.vram[addr as usize] = val;
                Some(())
            }
            0x20_0002 => {
                self.ctrl.set_high(val);
                log::debug!("high {:?}", self.ctrl);
                self.ctrl.set_reset(false);
                //self.ctrl.set_sense(0);
                Some(())
            }
            0x20_0003 => {
                self.ctrl.set_low(val);
                log::debug!("low {:?}", self.ctrl);
                self.ctrl.set_reset(false);
                //self.ctrl.set_sense(0);
                Some(())
            }
            0x20_0008 => Some(self.base.set_be0(val)),
            0x20_0009 => Some(self.base.set_be1(val)),
            0x20_000A => Some(self.base.set_be2(val)),
            0x20_000B => Some(self.base.set_be3(val)),
            0x20_000C => Some(self.stride.set_be0(val)),
            0x20_000D => Some(self.stride.set_be1(val)),
            0x20_000E => Some(self.stride.set_be2(val)),
            0x20_000F => Some(self.stride.set_be3(val)),
            // CRTC
            0x20_013C => {
                self.vblank_enable = val & (1 << 1) == 0;
                log::debug!("Vblank enable {:?}", self.vblank_enable);
                Some(())
            }
            // IRQ clear
            0x20_0148 => {
                self.vblank_irq = false;
                Some(())
            }
            0x20_0149..=0x20_014B => Some(()),
            // RAMDAC
            0x20_0204..=0x20_0207 => {
                self.clut_addr[addr as usize - 0x200204] = val;
                Some(())
            }
            0x20_020B => Some(self.ramdac_ctrl.0 = val),
            _ => None,
        }
    }
}

impl Tickable for Mdc12 {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.vblank_ticks += ticks;
        if self.vblank_ticks > 16_000_000 / 60 {
            // TODO attach renderer to card
            self.render.set();
            self.vblank_ticks = 0;
            if self.vblank_enable {
                self.vblank_irq = true;
            }
        }
        Ok(ticks)
    }
}

impl Debuggable for Mdc12 {
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
                ]
            ),
            dbgprop_enum!("BPP", self.bpp()),
            dbgprop_bool!("VBlank enable", self.vblank_enable),
        ]
    }
}

impl Display for Mdc12 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Macintosh Display Card 8-24")
    }
}
