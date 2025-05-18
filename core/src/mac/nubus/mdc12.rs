use anyhow::Result;
use proc_bitfield::bitfield;

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Field32, LatchingEvent, Word};

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

#[derive(Eq, PartialEq, Clone, Copy, strum::IntoStaticStr)]
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

impl ToString for Mdc12 {
    fn to_string(&self) -> String {
        "Macintosh Display Card 8-24".to_string()
    }
}
