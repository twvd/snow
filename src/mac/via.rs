use crate::bus::{Address, BusMember};
use crate::types::Byte;

use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// VIA Register A (for classic models)
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Sound volume
        pub sound: u8 @ 0..2,

        /// Sound buffer
        /// (true = main, false = alternate)
        pub sndpg2: bool @ 3,

        /// ROM overlay map is used when set
        pub overlay: bool @ 4,

        /// Disk SEL line
        pub headsel: bool @ 5,

        /// Video page to be used by video circuitry
        /// (true = main, false = alternate)
        pub page2: bool @ 6,

        /// SCC Wait/Request (false)
        pub sccwrreq: bool @ 7,
    }
}

bitfield! {
    /// VIA Register B (for classic models)
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterB(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// RTC data line
        pub rtcdata: bool @ 0,

        /// RTC clock line
        pub rtcclk: bool @ 1,

        /// RTC enabled
        pub rtcenb: bool @ 2,

        /// Mouse switch
        pub sw: bool @ 3,

        /// Mouse X2
        pub x2: bool @ 4,

        /// Mouse Y2
        pub y2: bool @ 5,

        /// HBlank
        pub h4: bool @ 6,

        /// Sound enable
        pub sndenb: bool @ 7,
    }
}

bitfield! {
    /// VIA Auxiliary Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterACR(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
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
    /// VIA Interrupt flag/enable registers
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterIRQ(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// One-second interrupt
        pub onesec: bool @ 0,

        /// Vertical blank
        pub vblank: bool @ 1,

        /// Keyboard data ready
        pub kbdready: bool @ 2,

        /// Keyboard data
        pub kbddata: bool @ 3,

        /// Keyboard clock
        pub kbdclock: bool @ 4,

        /// Timer T2
        pub t2: bool @ 5,

        /// Timer T1
        pub t1: bool @ 6,
    }
}

/// Synertek SY6522 Versatile Interface Adapter
pub struct Via {
    pub a: RegisterA,
    pub b: RegisterB,
    pub irq_enable: RegisterIRQ,
    pub irq_flag: RegisterIRQ,
    pub acr: RegisterACR,
}

impl Via {
    pub fn new() -> Self {
        Self {
            a: RegisterA(1 << 4),
            b: RegisterB(0),
            irq_enable: RegisterIRQ(0),
            irq_flag: RegisterIRQ(0),
            acr: RegisterACR(0),
        }
    }
}

impl BusMember<Address> for Via {
    fn read(&self, addr: Address) -> Option<Byte> {
        match addr & 0xFFFF {
            // Register B
            0xE1FE => {
                println!("read b");
                // TODO remove RTC stub
                Some(self.b.0 & 0xF0)
            }
            // Interrupt flag register
            0xFBFE => {
                let mut val = (self.irq_flag.0 & 0x7F) & self.irq_enable.0;
                if val > 0 {
                    val |= 0x80;
                }
                Some(val)
            }
            // Register A
            0xFFFE => Some(self.a.0),
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr & 0xFFFF {
            // Register B
            0xE1FE => {
                self.b.0 = val;
                dbg!(&self.b);
                Some(())
            }
            // Interrupt flag register
            0xFBFE => {
                self.irq_flag.0 &= !(val & 0x7F);
                dbg!(&self.irq_enable);
                dbg!(&self.irq_flag);
                Some(())
            }
            // Interrupt enable register
            0xFDFE => {
                if val & 0x80 != 0 {
                    // Enable
                    self.irq_enable.0 |= val & 0x7F;
                } else {
                    // Disable
                    self.irq_enable.0 &= !(val & 0x7F);
                }
                dbg!(&self.irq_enable);
                Some(())
            }
            // Register A
            0xFFFE => {
                self.a.0 = val;
                dbg!(&self.a);
                Some(())
            }
            _ => None,
        }
    }
}
