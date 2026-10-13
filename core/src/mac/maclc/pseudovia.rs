//! Registers of the V8 gate array, which take the place of VIA 2
//!
//! This is not a 6522: there are no timers, no shift register and no data
//! direction registers. Only A4, A1 and A0 are decoded.
//!
//! The Sonora gate array has the same registers, in which the expansion
//! register is its RAM configuration register. It decodes A3 and A2 too,
//! for two more registers.

use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::types::Byte;

use log::*;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// Data register B
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Address map (false = 24-bit, true = 32-bit)
        pub fc3: bool @ 3,
    }
}

bitfield! {
    /// Expansion register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterExp(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// 560x384 video mode for the Apple IIe card
        pub a2mode: bool @ 0,

        /// 256 KB of video RAM (512 KB if clear)
        pub vram_256k: bool @ 1,

        /// Video is refreshed from video RAM (from main RAM if clear)
        pub vram_in: bool @ 2,

        /// Size of the soldered RAM bank (true = 2 MB, false = 4 MB)
        pub ram_soldered_2mb: bool @ 5,

        /// Size of the SIMM RAM bank (0, 2, 4, 8 MB)
        pub ram_simm: u8 @ 6..=7,
    }
}

bitfield! {
    /// Slot interrupt flag (active low) and enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterSlot(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Expansion slot
        pub slot: bool @ 5,

        /// Vertical blanking of the built-in video
        pub vblank: bool @ 6,
    }
}

bitfield! {
    /// Interrupt flag and enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub scsi_drq: bool @ 0,

        /// Any enabled slot interrupt
        pub any_slot: bool @ 1,

        pub scsi_irq: bool @ 3,

        pub asc: bool @ 4,

        /// Any enabled interrupt (read), set/clear (write)
        pub any: bool @ 7,
    }
}

bitfield! {
    /// Monitor parameters register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterMonP(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Video depth
        pub depth: u8 @ 0..=2,

        /// Monitor sense lines
        pub sense: u8 @ 3..=5,
    }
}

/// Bits of the slot registers which are implemented
const SLOT_MASK: u8 = 0x78;
/// Interrupt flags which can assert the interrupt output
const IRQ_MASK: u8 = 0x1B;
/// Bits of data register B which read as 1
const B_ONES: u8 = 0x47;

const REG_B: Address = 0x00;
const REG_EXP: Address = 0x01;
const REG_SLOT_IFR: Address = 0x02;
const REG_IFR: Address = 0x03;
const REG_VRAM: Address = 0x04;
const REG_SPEED: Address = 0x05;
const REG_MONP: Address = 0x10;
const REG_SLOT_IER: Address = 0x12;
const REG_IER: Address = 0x13;

#[derive(Serialize, Deserialize)]
pub struct PseudoVia {
    b: RegisterB,
    exp: RegisterExp,
    monp: RegisterMonP,
    slot_ier: RegisterSlot,
    ier: RegisterIRQ,

    /// Vertical blanking interrupt, set at the start of the vertical blanking
    /// and cleared by writing the slot interrupt flag register
    vblank: bool,

    scsi_irq: bool,
    scsi_drq: bool,
    asc_irq: bool,

    /// Monitor sense lines
    sense: u8,

    /// Bits of the address which the gate array decodes
    addr_mask: Address,

    /// Video RAM configuration register of the Sonora
    vram: u8,

    /// CPU speed register of the Sonora
    speed: u8,
}

impl PseudoVia {
    pub fn new(sense: u8, addr_mask: Address) -> Self {
        Self {
            b: RegisterB(0),
            exp: RegisterExp(0),
            monp: RegisterMonP(0),
            slot_ier: RegisterSlot(0),
            ier: RegisterIRQ(0),
            vblank: false,
            scsi_irq: false,
            scsi_drq: false,
            asc_irq: false,
            sense,
            addr_mask,
            vram: 0,
            speed: 0,
        }
    }

    /// RESET line. What it clears has not been verified on hardware, this
    /// keeps the RAM configuration and disables the interrupts.
    pub fn reset(&mut self) {
        self.slot_ier = RegisterSlot(0);
        self.ier = RegisterIRQ(0);
        self.vblank = false;
    }

    pub fn b(&self) -> RegisterB {
        self.b
    }

    pub fn exp(&self) -> RegisterExp {
        self.exp
    }

    pub fn monp(&self) -> RegisterMonP {
        self.monp
    }

    /// Start of the vertical blanking
    pub fn set_vblank(&mut self) {
        self.vblank = true;
    }

    pub fn set_scsi(&mut self, irq: bool, drq: bool) {
        self.scsi_irq = irq;
        self.scsi_drq = drq;
    }

    pub fn set_asc_irq(&mut self, level: bool) {
        self.asc_irq = level;
    }

    fn slot_ifr(&self) -> RegisterSlot {
        // The expansion slot is not implemented
        RegisterSlot(!0x80).with_vblank(!self.vblank)
    }

    /// Interrupt flags, without the summary bit. They all follow their source.
    fn ifr(&self) -> RegisterIRQ {
        RegisterIRQ(0)
            .with_scsi_drq(self.scsi_drq)
            .with_any_slot(!self.slot_ifr().0 & self.slot_ier.0 & SLOT_MASK != 0)
            .with_scsi_irq(self.scsi_irq)
            .with_asc(self.asc_irq)
    }

    /// Level of the interrupt output
    pub fn irq(&self) -> bool {
        self.ifr().0 & self.ier.0 & IRQ_MASK != 0
    }

    /// Writes a register in which bit 7 selects setting or clearing of the
    /// other bits set in the written value
    fn write_set_clear(reg: u8, val: Byte) -> u8 {
        if val & 0x80 != 0 {
            reg | (val & 0x7F)
        } else {
            reg & !val
        }
    }
}

impl BusMember<Address> for PseudoVia {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr & self.addr_mask {
            REG_B => Some(self.b.0 | B_ONES),
            REG_EXP => Some(self.exp.0),
            REG_VRAM => Some(self.vram),
            REG_SPEED => Some(self.speed),
            REG_SLOT_IFR => Some(self.slot_ifr().0),
            REG_IFR => Some(self.ifr().with_any(self.irq()).0),
            REG_MONP => Some(self.monp.with_sense(self.sense).0),
            REG_SLOT_IER => Some(self.slot_ier.0),
            REG_IER => Some(self.ier.0),
            _ => Some(0),
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr & self.addr_mask {
            REG_B => self.b = RegisterB(val & !B_ONES),
            REG_EXP => {
                if RegisterExp(val) != self.exp {
                    debug!("Expansion register: {:02X}", val);
                }
                self.exp = RegisterExp(val);
            }
            REG_VRAM => {
                debug!("Video RAM configuration register: {:02X}", val);
                self.vram = val;
            }
            REG_SPEED => {
                debug!("CPU speed register: {:02X}", val);
                self.speed = val;
            }
            REG_SLOT_IFR => {
                if RegisterSlot(val).vblank() {
                    self.vblank = false;
                }
            }
            // The flags follow their source, there is nothing to clear
            REG_IFR => (),
            REG_MONP => self.monp = RegisterMonP(val),
            REG_SLOT_IER => {
                self.slot_ier =
                    RegisterSlot(Self::write_set_clear(self.slot_ier.0, val) & SLOT_MASK);
            }
            REG_IER => self.ier = RegisterIRQ(Self::write_set_clear(self.ier.0, val)),
            _ => (),
        }
        Some(())
    }
}

impl Debuggable for PseudoVia {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::dbgprop_byte_bin;
        use crate::debuggable::*;

        vec![
            dbgprop_byte_bin!("Data register B", self.b.0 | B_ONES),
            dbgprop_byte_bin!("Expansion register", self.exp.0),
            dbgprop_byte_bin!("Slot interrupt flags", self.slot_ifr().0),
            dbgprop_byte_bin!("Slot interrupt enable", self.slot_ier.0),
            dbgprop_byte_bin!("Interrupt flags", self.ifr().with_any(self.irq()).0),
            dbgprop_byte_bin!("Interrupt enable", self.ier.0),
            dbgprop_byte_bin!("Monitor parameters", self.monp.with_sense(self.sense).0),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::maclc::{sonora, v8};

    #[test]
    fn mirrors() {
        let mut v8 = PseudoVia::new(0b110, v8::PSEUDOVIA_MASK);

        v8.write(REG_EXP, 0xE6);
        for addr in [0x01, 0x05, 0x0D, 0x201, 0x1A01] {
            assert_eq!(v8.read(addr), Some(0xE6), "{:04X}", addr);
        }
        assert_eq!(v8.read(0x1C13), v8.read(REG_IER));
    }

    #[test]
    fn sonora_registers() {
        let mut sonora = PseudoVia::new(0b110, sonora::PSEUDOVIA_MASK);

        sonora.write(REG_EXP, 0x46);
        sonora.write(REG_VRAM, 0x01);
        sonora.write(REG_SPEED, 0x02);
        assert_eq!(sonora.read(REG_EXP), Some(0x46));
        assert_eq!(sonora.read(REG_VRAM), Some(0x01));
        assert_eq!(sonora.read(REG_SPEED), Some(0x02));
        assert_eq!(sonora.read(0x1A04), Some(0x01));
    }

    #[test]
    fn data_b() {
        let mut v8 = PseudoVia::new(0b110, v8::PSEUDOVIA_MASK);

        assert_eq!(v8.read(REG_B), Some(0x47));
        v8.write(REG_B, 0xFF);
        assert!(v8.b().fc3());
        v8.write(REG_B, 0x00);
        assert!(!v8.b().fc3());
    }

    #[test]
    fn monitor_sense() {
        let mut v8 = PseudoVia::new(0b010, v8::PSEUDOVIA_MASK);

        v8.write(REG_MONP, 0x3B);
        assert_eq!(v8.read(REG_MONP), Some(0x13));
        assert_eq!(v8.monp().depth(), 3);
    }

    #[test]
    fn enable_set_clear() {
        let mut v8 = PseudoVia::new(0b110, v8::PSEUDOVIA_MASK);

        v8.write(REG_IER, 0x89);
        assert_eq!(v8.read(REG_IER), Some(0x09));
        v8.write(REG_IER, 0x01);
        assert_eq!(v8.read(REG_IER), Some(0x08));

        v8.write(REG_SLOT_IER, 0xFF);
        assert_eq!(v8.read(REG_SLOT_IER), Some(0x78));
        v8.write(REG_SLOT_IER, 0x40);
        assert_eq!(v8.read(REG_SLOT_IER), Some(0x38));
    }

    #[test]
    fn vblank() {
        let mut v8 = PseudoVia::new(0b110, v8::PSEUDOVIA_MASK);

        assert_eq!(v8.read(REG_SLOT_IFR), Some(0x7F));
        v8.set_vblank();
        assert_eq!(v8.read(REG_SLOT_IFR), Some(0x3F));
        assert_eq!(v8.read(REG_IFR), Some(0x00));
        assert!(!v8.irq());

        // Slot enable gates the summary flag, the enable register the output
        v8.write(REG_SLOT_IER, 0xC0);
        assert_eq!(v8.read(REG_IFR), Some(0x02));
        assert!(!v8.irq());
        v8.write(REG_IER, 0x82);
        assert_eq!(v8.read(REG_IFR), Some(0x82));
        assert!(v8.irq());

        // Writing the summary flag does not acknowledge
        v8.write(REG_IFR, 0x82);
        assert!(v8.irq());
        v8.write(REG_SLOT_IFR, 0x40);
        assert!(!v8.irq());
        assert_eq!(v8.read(REG_SLOT_IFR), Some(0x7F));
    }

    #[test]
    fn level_sources() {
        let mut v8 = PseudoVia::new(0b110, v8::PSEUDOVIA_MASK);

        v8.write(REG_IER, 0x99);
        v8.set_scsi(true, false);
        assert_eq!(v8.read(REG_IFR), Some(0x88));
        v8.set_scsi(false, true);
        assert_eq!(v8.read(REG_IFR), Some(0x81));
        v8.set_scsi(false, false);
        v8.set_asc_irq(true);
        assert_eq!(v8.read(REG_IFR), Some(0x90));
        v8.set_asc_irq(false);
        assert!(!v8.irq());
    }

    #[test]
    fn reset() {
        let mut v8 = PseudoVia::new(0b110, v8::PSEUDOVIA_MASK);

        v8.write(REG_EXP, 0xE4);
        v8.write(REG_IER, 0x9B);
        v8.write(REG_SLOT_IER, 0xC0);
        v8.set_vblank();
        v8.reset();
        assert_eq!(v8.read(REG_EXP), Some(0xE4));
        assert_eq!(v8.read(REG_IER), Some(0x00));
        assert_eq!(v8.read(REG_SLOT_IER), Some(0x00));
        assert_eq!(v8.read(REG_SLOT_IFR), Some(0x7F));
    }
}
