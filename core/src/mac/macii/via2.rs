use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::mac::MacModel;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, Field16};

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// VIA Register A
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Expansion slot $9 IRQ
        pub v2irq1: bool @ 0,
        /// Expansion slot $A IRQ
        pub v2irq2: bool @ 1,
        /// Expansion slot $B IRQ
        pub v2irq3: bool @ 2,
        /// Expansion slot $C IRQ
        pub v2irq4: bool @ 3,
        /// Expansion slot $D IRQ
        pub v2irq5: bool @ 4,
        /// Expansion slot $E IRQ
        pub v2irq6: bool @ 5,

        pub v2irqs: u8 @ 0..=5,

        /// RAM-size (output)
        pub v2ram0: u8 @ 6..=7,
    }
}

bitfield! {
    /// VIA Register B
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// 0 = Disable CPU instruction/data cache
        pub v2cdis: bool @ 0,

        /// 0 = NuBus transactions locked out
        pub v2buslk: bool @ 1,

        /// 0 = power off
        pub v2poweroff: bool @ 2,

        /// PMMU/AMU control
        /// For AMU: 0 = 24-bit
        pub vfc3: bool @ 3,

        /// NuBus transfer acknowledge
        pub v2tmxa: u8 @ 4..=5,

        pub v2sndext: bool @ 6,
        pub v2vbl: bool @ 7,

    }
}

bitfield! {
    /// VIA Auxiliary Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterACR(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Input latch register A
        pub ralatch: bool @ 0,

        /// Input latch register B
        pub rblatch: bool @ 1,

        /// Keyboard bit-shift operation
        pub kbd: u8 @ 2..=4,

        /// Timer T2 interrupts
        pub t2: bool @ 5,

        /// Timer T1 interrupts
        pub t1: u8 @ 6..=7,
    }
}

bitfield! {
    /// VIA Peripheral Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterPCR(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// VBlank
        pub vblank: bool @ 0,

        /// One second interrupt
        pub onesec: u8 @ 1..=3,

        /// Keyboard clock
        pub kbdclk: bool @ 4,

        /// Keyboard bit-shift operation
        pub kbddata: u8 @ 5..=7,
    }
}

bitfield! {
    /// VIA Interrupt flag/enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Cleared on Register A read/write
        pub scsi_drq: bool @ 0,

        /// Cleared on Register A read/write
        pub slot: bool @ 1,

        /// Keyboard data ready
        /// Cleared on read/write shift reg
        pub kbdready: bool @ 2,

        /// Cleared on Register B read/write
        pub scsi_irq: bool @ 3,

        /// Cleared on Register B read/write
        pub asc: bool @ 4,

        /// Timer T2
        /// Cleared on read of T2 counter LSB or write of T2 counter MSB
        pub t2: bool @ 5,

        /// Timer T1
        /// Cleared on read of T1 counter LSB or write of T1 counter MSB
        pub t1: bool @ 6,
    }
}

/// Synertek SY6522 Versatile Interface Adapter
#[derive(Serialize, Deserialize)]
pub struct Via2 {
    /// The currently emulated Macintosh model
    #[allow(dead_code)]
    model: MacModel,

    /// Register A, outputs
    pub a_out: RegisterA,

    /// Register A, inputs
    pub a_in: RegisterA,

    /// Data Direction Register A
    pub ddra: RegisterA,

    /// Register B, outputs
    pub b_out: RegisterB,

    /// Register B, inputs
    pub b_in: RegisterB,

    /// Data Direction Register B
    pub ddrb: RegisterB,

    /// Interrupt Enable Register
    pub ier: RegisterIRQ,

    /// Interrupt Flag Register
    pub ifr: RegisterIRQ,

    /// Peripheral Control Register
    pub pcr: RegisterPCR,

    /// Auxiliary Control Register
    pub acr: RegisterACR,

    /// Timer 2 Counter
    pub t2cnt: Field16,

    /// Timer 2 latch
    pub t2latch: Field16,

    kbdshift_in: u8,
    kbdshift_out: u8,
    kbdshift_out_time: Ticks,

    t1_enable: bool,
    t2_enable: bool,

    /// Timer 1 Counter
    pub t1cnt: Field16,

    /// Timer 1 latch
    pub t1latch: Field16,
}

impl Via2 {
    pub fn new(model: MacModel) -> Self {
        Self {
            model,
            a_out: RegisterA(0),
            b_out: RegisterB(0),
            a_in: RegisterA(0xFF),
            b_in: model.via2_b_in(),
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

            kbdshift_in: 0,
            kbdshift_out: 0,
            kbdshift_out_time: 0,
        }
    }
}

impl BusMember<Address> for Via2 {
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
            // Timer 1 counter MSB
            0x05 => Some(self.t1cnt.msb()),

            // Timer 1 latch LSB
            0x06 => Some(self.t1latch.lsb()),
            // Timer 1 latch MSB
            0x07 => Some(self.t1latch.msb()),

            // Register B
            0x00 => {
                self.ifr.set_scsi_irq(false);
                self.ifr.set_asc(false);

                Some((self.b_in.0 & !self.ddrb.0) | (self.b_out.0 & self.ddrb.0))
            }

            // Register B Data Direction
            0x02 => Some(self.ddrb.0),

            // Register A Data Direction
            0x03 => Some(self.ddra.0),

            // Keyboard shift register
            0x0A => {
                self.ifr.set_kbdready(false);
                Some(self.kbdshift_in)
            }

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
                self.ifr.set_slot(false);
                self.ifr.set_scsi_drq(false);

                Some((self.a_in.0 & !self.ddra.0) | (self.a_out.0 & self.ddra.0))
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match (addr >> 9) & 0x0F {
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

            // Keyboard shift register
            0x0A => {
                log::warn!("Shifter write on VIA2");
                self.ifr.set_kbdready(false);

                self.kbdshift_out = val;
                Some(())
            }

            // Register B
            0x00 => {
                self.ifr.set_scsi_irq(false);
                self.ifr.set_asc(false);

                self.b_out.0 = val;
                Some(())
            }

            // Register B Data Direction
            0x02 => {
                self.ddrb.0 = val;
                Some(())
            }

            // Register A Data Direction
            0x03 => Some(self.ddra.0 = val),

            // Auxiliary Control Register
            0x0B => Some(self.acr.0 = val),

            // Peripheral Control Register
            0x0C => Some(self.pcr.0 = val),

            // Interrupt Flag register
            0x0D => {
                self.ifr.0 &= !(val & 0x7F);
                Some(())
            }

            // Interrupt Enable register
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
                self.ifr.set_slot(false);
                self.ifr.set_scsi_drq(false);

                self.a_out.0 = val;
                Some(())
            }

            _ => None,
        }
    }
}

impl Tickable for Via2 {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks> {
        // Timer 1
        let t1ovf;
        (self.t1cnt.0, t1ovf) = self.t1cnt.0.overflowing_sub(ticks.try_into()?);

        if t1ovf && self.t1_enable {
            self.ifr.set_t1(true);
            match self.acr.t1() {
                // Single shot mode
                0 | 2 => {
                    self.t1_enable = false;
                }
                1 | 3 => {
                    // Free running mode
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

impl Debuggable for Via2 {
    fn get_debug_properties(&self) -> crate::debuggable::DebuggableProperties {
        use crate::debuggable::*;
        use crate::{dbgprop_bool, dbgprop_byte_bin, dbgprop_group, dbgprop_udec, dbgprop_word};

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
            dbgprop_group!(
                "Shifter",
                vec![
                    dbgprop_byte_bin!("Input", self.kbdshift_in),
                    dbgprop_byte_bin!("Output", self.kbdshift_out),
                    dbgprop_udec!("Output timer", self.kbdshift_out_time),
                ]
            ),
            dbgprop_byte_bin!("Interrupt Enable (IER)", self.ier.0),
            dbgprop_byte_bin!("Interrupt Flags (IFR)", self.ifr.0),
            dbgprop_byte_bin!("Peripheral Control (PCR)", self.pcr.0),
            dbgprop_byte_bin!("Auxiliary Control (ACR)", self.acr.0),
        ]
    }
}
