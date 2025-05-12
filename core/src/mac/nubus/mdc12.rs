use anyhow::Result;
use proc_bitfield::bitfield;

use crate::{
    bus::{Address, BusMember},
    tickable::{Tickable, Ticks},
    types::{LatchingEvent, Word},
};

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

/// Macintosh Display Card 1.2.341-0868
pub struct Mdc12 {
    rom: Vec<u8>,
    ctrl: CtrlReg,
    vblank_irq: bool,
    vblank_enable: bool,
    vblank_ticks: Ticks,
    pub vram: Vec<u8>,
    toggle: bool,
    clut_addr: [u8; 4],
    pub render: LatchingEvent,
}

impl Mdc12 {
    pub fn new() -> Self {
        Self {
            rom: std::fs::read("341-0868.bin").expect("Graphics card ROM file"),
            ctrl: CtrlReg(0),
            vblank_irq: false,
            vblank_enable: false,
            vram: vec![0; 0x1FFFFF],
            toggle: false,
            vblank_ticks: 0,
            clut_addr: [0; 4],
            render: LatchingEvent::default(),
        }
    }

    pub fn get_irq(&self) -> bool {
        self.vblank_irq
    }
}

impl BusMember<Address> for Mdc12 {
    fn read(&mut self, addr: Address) -> Option<u8> {
        // Assume normal slot, not super slot
        match addr & 0xFF_FFFF {
            0x00_0000..=0x1F_FFFF => Some(self.vram[addr as usize]),
            0x20_0002 => Some(self.ctrl.high()),
            0x20_0003 => Some(self.ctrl.low()),
            // CRTC beam position
            0x20_01C0..=0x20_01C3 => {
                if addr == 0x20_01C0 {
                    self.toggle = !self.toggle;
                }
                if self.toggle {
                    Some(0)
                } else {
                    Some(4)
                }
            }
            0x20_01CC..=0x20_01CF => Some(0),
            // RAMDAC
            0x20_0204..=0x20_0207 => Some(self.clut_addr[addr as usize - 0x200204]),
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
            0x00_0000..=0x1F_FFFF => {
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
