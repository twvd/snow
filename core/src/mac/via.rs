use crate::bus::{Address, BusMember};
use crate::mac::keyboard::Keyboard;
use crate::mac::rtc::Rtc;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, Field16};

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

/// Counter at which to trigger the one second interrupt
/// (counted on the E Clock)
const ONESEC_TICKS: Ticks = 783360;

/// Time until a keyboard response is created
///
/// Time for the host command to be clocked out is:
/// 840us + (180us [clock low] + 220us [clock high]) * 8 + 80us
///
/// Time for the keyboard response to be clocked in:
/// (160us [clock low] + 180us [clock high]) * 8
/// = 6840us, roughly 7ms.
const KEYBOARD_DELAY: Ticks = ONESEC_TICKS * 7 / 1000;

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
        pub sel: bool @ 5,

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

        /// Mouse switch (false = down)
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
    /// VIA Peripheral Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterPCR(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
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
    pub struct RegisterIRQ(pub u8): Debug, FromRaw, IntoRaw, DerefRaw {
        /// One-second interrupt
        /// Cleared on Register A read/write
        pub onesec: bool @ 0,

        /// Vertical blank
        /// Cleared on Register A read/write
        pub vblank: bool @ 1,

        /// Keyboard data ready
        /// Cleared on read/write shift reg
        pub kbdready: bool @ 2,

        /// Keyboard data
        /// Cleared on Register B read/write
        pub kbddata: bool @ 3,

        /// Keyboard clock
        /// Cleared on Register B read/write
        pub kbdclock: bool @ 4,

        /// Timer T2
        /// Cleared on read of T2 counter LSB or write of T2 counter MSB
        pub t2: bool @ 5,

        /// Timer T1
        /// Cleared on read of T1 counter LSB or write of T1 counter MSB
        pub t1: bool @ 6,
    }
}

/// Synertek SY6522 Versatile Interface Adapter
pub struct Via {
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

    /// Counter for the one-second timer
    onesec: Ticks,

    kbdshift_in: u8,
    kbdshift_out: u8,
    kbdshift_out_time: Ticks,

    t1_enable: bool,
    t2_enable: bool,

    /// Timer 1 Counter
    pub t1cnt: Field16,

    /// Timer 1 latch
    pub t1latch: Field16,

    pub keyboard: Keyboard,
    rtc: Rtc,
}

impl Via {
    pub fn new() -> Self {
        Self {
            a_out: RegisterA(0xFF),
            b_out: RegisterB(0xFF),
            a_in: RegisterA(0xFF),
            b_in: RegisterB(0xFF),
            ddra: RegisterA(0xFF),
            ddrb: RegisterB(0xFF),
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
            kbdshift_in: 0,
            kbdshift_out: 0,
            kbdshift_out_time: 0,

            keyboard: Keyboard::default(),
            rtc: Rtc::default(),
        }
    }
}

impl BusMember<Address> for Via {
    fn read(&mut self, addr: Address) -> Option<Byte> {
        match addr & 0xFFFF {
            // Timer 2 counter LSB
            0xF1FE => {
                self.ifr.set_t2(false);
                Some(self.t2cnt.lsb())
            }
            // Timer 2 counter MSB
            0xF3FE => Some(self.t2cnt.msb()),

            // Timer 1 counter LSB
            0xE9FE => {
                self.ifr.set_t1(false);
                Some(self.t1cnt.lsb())
            }
            // Timer 1 counter MSB
            0xEBFE => Some(self.t1cnt.msb()),

            // Timer 1 latch LSB
            0xEDFE => Some(self.t1latch.lsb()),
            // Timer 1 latch MSB
            0xEFFE => Some(self.t1latch.msb()),

            // Register B
            0xE1FE => {
                self.ifr.set_kbddata(false);
                self.ifr.set_kbdclock(false);

                Some((self.b_in.0 & !self.ddrb.0) | (self.b_out.0 & self.ddrb.0))
            }

            // Register B Data Direction
            0xE5FE => Some(self.ddrb.0),

            // Register A Data Direction
            0xE7FE => Some(self.ddra.0),

            // Keyboard shift register
            0xF5FE => {
                self.ifr.set_kbdready(false);
                Some(self.kbdshift_in)
            }

            // Auxiliary Control Register
            0xF7FE => Some(self.acr.0),

            // Peripheral Control Register
            0xF9FE => Some(self.pcr.0),

            // Interrupt Flag Register
            0xFBFE => {
                let mut val = self.ifr.0 & 0x7F;
                if self.ifr.0 & self.ier.0 != 0 {
                    val |= 0x80;
                }
                Some(val)
            }

            // Interrupt Enable Register
            0xFDFE => Some(self.ier.0 | 0x80),

            // Register A
            0xFFFE => {
                // The SCC write request bit is used for checking if there's data ready
                // from the SCC in critical sections where SCC interrupts are disabled.
                // Reporting 1 signals that there's no data ready.
                self.a_in.set_sccwrreq(true);

                self.ifr.set_vblank(false);
                self.ifr.set_onesec(false);

                Some((self.a_in.0 & !self.ddra.0) | (self.a_out.0 & self.ddra.0))
            }
            _ => None,
        }
    }

    fn write(&mut self, addr: Address, val: Byte) -> Option<()> {
        match addr & 0xFFFF {
            // Timer 2 counter LSB
            0xF1FE => Some(self.t2latch.set_lsb(val)),

            // Timer 2 counter MSB
            0xF3FE => {
                self.t2latch.set_msb(val);

                // Clear interrupt flag
                self.ifr.set_t2(false);

                // Start timer
                self.t2_enable = true;
                self.t2cnt = self.t2latch;
                Some(())
            }

            // Timer 1 counter LSB, Timer 1 latch LSB
            0xE9FE | 0xEDFE => Some(self.t1latch.set_lsb(val)),

            // Timer 1 latch MSB
            0xEFFE => {
                self.t1latch.set_msb(val);

                // Clear interrupt flag
                self.ifr.set_t1(false);

                Some(())
            }

            // Timer 1 counter MSB
            0xEBFE => {
                self.t1cnt.set_lsb(self.t1latch.lsb());
                self.t1cnt.set_msb(val);

                // Clear interrupt flag
                self.ifr.set_t1(false);

                // Start timer
                self.t1_enable = true;
                Some(())
            }

            // Keyboard shift register
            0xF5FE => {
                self.ifr.set_kbdready(false);

                self.kbdshift_out = val;
                self.kbdshift_out_time = KEYBOARD_DELAY;
                Some(())
            }

            // Register B
            0xE1FE => {
                self.ifr.set_kbddata(false);
                self.ifr.set_kbdclock(false);

                self.b_out.0 = val;
                let rtcin = self.rtc.io(
                    self.b_out.rtcenb(),
                    self.b_out.rtcclk(),
                    self.b_out.rtcdata(),
                );
                self.b_in.set_rtcdata(rtcin);
                Some(())
            }

            // Register B Data Direction
            0xE5FE => Some(self.ddrb.0 = val),

            // Register A Data Direction
            0xE7FE => Some(self.ddra.0 = val),

            // Auxiliary Control Register
            0xF7FE => Some(self.acr.0 = val),

            // Peripheral Control Register
            0xF9FE => Some(self.pcr.0 = val),

            // Interrupt Flag register
            0xFBFE => {
                self.ifr.0 &= !(val & 0x7F);
                Some(())
            }

            // Interrupt Enable register
            0xFDFE => {
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
            0xFFFE => {
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
        // This is ticked on the E Clock
        self.onesec += ticks;

        // One second interrupt
        if self.onesec >= ONESEC_TICKS {
            self.onesec -= ONESEC_TICKS;
            self.ifr.set_onesec(true);
            self.rtc.second();
        }

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

        // Keyboard response
        if self.kbdshift_out_time > 0 {
            self.kbdshift_out_time = self.kbdshift_out_time.saturating_sub(ticks);
            if self.kbdshift_out_time == 0 {
                self.kbdshift_in = self.keyboard.cmd(self.kbdshift_out)?;
                self.acr.set_kbd(0);
                self.ifr.set_kbdready(true);
            }
        }

        Ok(ticks)
    }
}
