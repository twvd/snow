//! VIA implementation for the Macintosh Portable/PowerBook 100

use crate::bus::Address;
use crate::bus::BusMember;
use crate::debuggable::DebuggableProperty;
use crate::debuggable::DebuggablePropertyValue;
use crate::debuggable::{Debuggable, DebuggableProperties};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, Field16};
use crate::{dbgprop_bool, dbgprop_byte_bin, dbgprop_group, dbgprop_udec, dbgprop_word};
use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

/// Counter at which to trigger the one-second interrupt
/// TODO
const ONESEC_TICKS: Ticks = 783360;

bitfield! {
    /// VIA Register A
    #[derive(Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub pmd0: bool @ 0,
        pub pmd1: bool @ 1,
        pub pmd2: bool @ 2,
        pub pmd3: bool @ 3,
        pub pmd4: bool @ 4,
        pub pmd5: bool @ 5,
        pub pmd6: bool @ 6,
        pub pmd7: bool @ 7,
    }
}

bitfield! {
    /// VIA Register B
    #[derive(Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub pmreq: bool @ 0,
        pub pmack: bool @ 1,
        pub test: bool @ 2,
        pub sync: bool @ 3,
        pub drivesel: bool @ 4,
        pub headsel: bool @ 5,
        pub sndext: bool @ 6,
        pub sccwrreq: bool @ 7,
        pub sndenb: bool @ 7,
    }
}

bitfield! {
    /// VIA Auxilary Control Register
    #[derive(Serialize, Deserialize)]
    pub struct RegisterACR(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Input latch register A
        pub ralatch: bool @ 0,

        /// Input latch register B
        pub rblatch: bool @ 1,

        /// Timer T2 interrupts
        pub t2: bool @ 5,

        /// Timer T1 interrupts
        pub t1: u8 @ 6..=7,
    }
}

bitfield! {
    /// VIA Peripheral Control Register
    #[derive(Serialize, Deserialize)]
    pub struct RegisterPCR(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// VBlank/60.15Hz
        pub vblank: bool @ 0,
        /// One-second interrupt
        pub onesec: u8 @ 1..=3,
        /// Power manager interrupt
        pub pmgr: bool @ 4,
        /// SCSI IRQ interrupt
        pub scsi_irq: u8 @ 5..=7,
    }
}

bitfield! {
    /// VIA Interrupt flag/enable registers
    #[derive(Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        pub onesec: bool @ 0,
        pub vblank: bool @ 1,
        pub scsi_irq: bool @ 3,
        pub pmgr: bool @ 4,
        pub t2: bool @ 5,
        pub t1: bool @ 6,
        pub irq: bool @ 7,
    }
}

#[derive(Serialize, Deserialize)]
pub struct Via {
    pub a_out: RegisterA,
    pub a_in: RegisterA,
    pub ddra: RegisterA,
    pub b_out: RegisterB,
    pub b_in: RegisterB,
    pub ddrb: RegisterB,
    pub ier: RegisterIRQ,
    pub ifr: RegisterIRQ,
    pub pcr: RegisterPCR,
    pub acr: RegisterACR,
    pub t2cnt: Field16,
    pub t2latch: Field16,

    onesec: Ticks,

    t1_enable: bool,
    t2_enable: bool,

    pub t1cnt: Field16,
    pub t1latch: Field16,
}

impl Via {
    pub fn new() -> Self {
        Self {
            a_out: RegisterA(0),
            b_out: RegisterB(0),
            a_in: RegisterA(0xFF),
            b_in: RegisterB(0xFF),
            ddra: RegisterA(0),
            ddrb: RegisterB(0),
            ier: RegisterIRQ(0),
            ifr: RegisterIRQ(0),
            acr: RegisterACR(0),
            pcr: RegisterPCR(0),

            t2cnt: Field16(0),
            t2latch: Field16(0),
            t2_enable: false,

            t1cnt: Field16(0),
            t1latch: Field16(0),
            t1_enable: false,

            onesec: 0,
        }
    }
}

impl BusMember<Address> for Via {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match (addr >> 9) & 0xF {
            // Timer 2 counter LSB
            0x08 => {
                self.ifr.set_t2(false);
                Some(self.t2cnt.lsb())
            }
            // Timer 2 counter MSB
            0x09 => Some(self.t2cnt.msb()),

            // Timer 1 counter LSB
            0x04 => {
                self.ifr.set_t1(false);
                Some(self.t1cnt.lsb())
            }
            // timer 1 counter MSB
            0x05 => Some(self.t1cnt.msb()),

            // Timer 1 latch LSB
            0x06 => Some(self.t1latch.lsb()),
            // Timer 1 latch MSB
            0x07 => Some(self.t1latch.msb()),

            // Register B
            0x00 => {
                // The SCC write request bit is used for checking if there's data ready
                // from the SCC in critical sections where SCC interrupts are disabled.
                // Reporting 1 signals that there's no data ready.
                self.b_in.set_sccwrreq(true);

                Some((self.b_in.0 & !self.ddrb.0) | (self.b_out.0 & self.ddrb.0))
            }

            // Register B Data Direction
            0x02 => Some(self.ddrb.0),

            // Register A Data Direction
            0x03 => Some(self.ddra.0),

            // TODO
            0x0A => None,

            // Auxiliary Control Register
            0x0B => Some(self.acr.0),

            // Peripheral Control Register
            0x0C => Some(self.pcr.0),

            // Interrupt Flag Register
            0x0D => {
                let mut val = self.ifr.0 & 0x7F;
                if self.ifr.0 & self.ier.0 != 0 {
                    val |= 0x80;
                }
                Some(val)
            }

            // Interrupt Enable Register
            0x0E => Some(self.ier.0 | 0x80),

            // Register A
            0x01 | 0x0F => {
                self.ifr.set_vblank(false);
                self.ifr.set_onesec(false);

                Some((self.a_in.0 & !self.ddra.0) | (self.a_out.0 & self.ddra.0))
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match (addr >> 9) & 0xF {
            // Timer 2 counter LSB
            0x08 => Some(self.t2latch.set_lsb(val)),

            // Timer 2 counter MSB
            0x09 => {
                self.t2latch.set_msb(val);

                // Clear interrupt flag
                self.ifr.set_t2(false);

                // Start timer
                self.t2_enable = true;
                self.t2cnt = self.t2latch;
                Some(())
            }

            // Timer 1 counter LSB, Timer 1 latch LSB
            0x04 | 0x06 => Some(self.t1latch.set_lsb(val)),

            // Timer 1 latch MSB
            0x07 => {
                self.t1latch.set_msb(val);

                // Clear interrupt flag
                self.ifr.set_t1(false);

                Some(())
            }

            // Timer 1 counter MSB
            0x05 => {
                self.t1latch.set_msb(val);
                self.t1cnt.0 = self.t1latch.0;

                // Clear interrupt flag
                self.ifr.set_t1(false);

                // Start timer
                self.t1_enable = true;
                Some(())
            }

            // TODO
            0x0A => None,

            // Register B
            0x00 => {
                self.b_out.0 = val;

                Some(())
            }

            // Register B Data Direction
            0x02 => Some(self.ddrb.0 = val),

            // Register A Data Direction
            0x03 => Some(self.ddra.0 = val),

            // Auxiliary Control Register
            0x0B => Some(self.acr.0 = val),

            // Peripheral Control Register
            0x0C => Some(self.pcr.0 = val),

            // Interrupt Flag Register
            0x0D => {
                self.ifr.0 &= !(val & 0x7F);
                Some(())
            }

            // Interrupt Enable Register
            0x0E => {
                let newflags = if val & 0x80 != 0 {
                    // Enable
                    RegisterIRQ(self.ier.0 | (val & 0x7F))
                } else {
                    // Disable
                    RegisterIRQ(self.ier.0 & !(val & 0x7F))
                };
                self.ier = newflags;
                Some(())
            }

            // Register A
            0x01 | 0x0F => {
                self.ifr.set_vblank(false);
                self.ifr.set_onesec(false);

                self.a_out.0 = val;
                Some(())
            }

            _ => None,
        }
    }
}

impl Tickable for Via {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        self.onesec += ticks;

        if self.onesec >= ONESEC_TICKS {
            self.onesec -= ONESEC_TICKS;
            self.ifr.set_onesec(true);
            // TODO self.rtc.second();
        }
        // Timer 1
        let t1ovf;
        (self.t1cnt.0, t1ovf) = self.t1cnt.overflowing_sub(ticks.try_into()?);

        if t1ovf && self.t1_enable {
            self.ifr.set_t1(true);
            match self.acr.t1() {
                // Single shot mode
                0 | 2 => {
                    self.t1_enable = false;
                }
                1 | 3 => {
                    self.t1cnt.0 = self.t1latch.0;
                }
                _ => unreachable!(),
            }
        }

        // Timer 2
        let t2ovf;
        (self.t2cnt.0, t2ovf) = self.t2cnt.0.overflowing_sub(ticks.try_into()?);

        if t2ovf && self.t2_enable {
            self.t2_enable = false;
            self.ifr.set_t2(true);
        }

        Ok(ticks)
    }
}

impl Debuggable for Via {
    fn get_debug_properties(&self) -> DebuggableProperties {
        vec![
            dbgprop_group!(
                "Register A",
                vec![
                    dbgprop_byte_bin!("DDRA", self.ddra.0),
                    dbgprop_byte_bin!("Inputs", self.a_in.0),
                    dbgprop_byte_bin!("Outputs", self.a_out.0),
                ]
            ),
            dbgprop_group!(
                "Register B",
                vec![
                    dbgprop_byte_bin!("DDRB", self.ddrb.0),
                    dbgprop_byte_bin!("Inputs", self.b_in.0),
                    dbgprop_byte_bin!("Outputs", self.b_out.0),
                ]
            ),
            dbgprop_group!(
                "Timer 1",
                vec![
                    dbgprop_word!("Counter", self.t1cnt.0),
                    dbgprop_word!("Latch", self.t1latch.0),
                    dbgprop_bool!("Armed", self.t1_enable)
                ]
            ),
            dbgprop_group!(
                "Timer 2",
                vec![
                    dbgprop_word!("Counter", self.t2cnt.0),
                    dbgprop_word!("Latch", self.t2latch.0),
                    dbgprop_bool!("Armed", self.t2_enable)
                ]
            ),
            dbgprop_udec!("One second timer", self.onesec),
            dbgprop_byte_bin!("Interrupt Enable (IER)", self.ier.0),
            dbgprop_byte_bin!("Interrupt Flags (IFR)", self.ifr.0),
            dbgprop_byte_bin!("Peripheral Control (PCR)", self.pcr.0),
            dbgprop_byte_bin!("Auxiliary Control (ACR)", self.acr.0),
        ]
    }
}
