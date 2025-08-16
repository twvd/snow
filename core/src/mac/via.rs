use crate::bus::{Address, BusMember};
use crate::debuggable::Debuggable;
use crate::mac::pluskbd::PlusKeyboard;
use crate::mac::rtc::Rtc;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, Field16};

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use super::adb::AdbTransceiver;
use super::MacModel;

/// Counter at which to trigger the one second interrupt
/// (counted on the E Clock)
const ONESEC_TICKS: Ticks = 783360;

const SHIFT_DELAY: Ticks = ONESEC_TICKS * 3 / 1000;

const ACR_SHIFT_OUT: u8 = 0b111;
const ACR_SHIFT_IN: u8 = 0b011;

bitfield! {
    /// VIA Register A (for classic models)
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterA(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Sound volume (non-ASC models)
        pub sound: u8 @ 0..=2,

        /// Model sense (Mac II)
        pub model: u8 @ 0..=2,

        /// Sound buffer
        /// (true = main, false = alternate)
        pub sndpg2: bool @ 3,

        /// ROM overlay map is used when set (512K and Plus)
        pub overlay: bool @ 4,

        /// Floppy drive select
        pub drivesel: bool @ 4,

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
    pub struct RegisterB(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// RTC data line
        pub rtcdata: bool @ 0,

        /// RTC clock line
        pub rtcclk: bool @ 1,

        /// RTC enabled
        pub rtcenb: bool @ 2,

        /// Mouse switch (false = down) (512K and Plus)
        pub sw: bool @ 3,

        /// ADB interrupt (SE+)
        pub adb_int: bool @ 3,

        /// Mouse X2 (512K and Plus)
        pub mouse_x2: bool @ 4,

        /// ADB state input 0 (ST0) (SE+)
        pub adb_st0: bool @ 4,

        /// Mouse Y2 (512K and Plus)
        pub mouse_y2: bool @ 5,

        /// ADB state input 1 (ST1) (SE+)
        pub adb_st1: bool @ 5,

        /// HBlank (512K and Plus)
        pub h4: bool @ 6,

        /// SCSI interrupt (false = enabled) (SE+)
        pub scsi_int: bool @ 6,

        /// Sound enable
        pub sndenb: bool @ 7,
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
    /// The currently emulated Macintosh model
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

    /// Counter for the one-second timer
    onesec: Ticks,

    /// Shift register
    sr: u8,

    kbdshift_in: u8,
    kbdshift_in_time: Ticks,
    kbdshift_out: u8,
    kbdshift_out_time: Ticks,

    t1_enable: bool,
    t2_enable: bool,

    /// Timer 1 Counter
    pub t1cnt: Field16,

    /// Timer 1 latch
    pub t1latch: Field16,

    pub keyboard: PlusKeyboard,
    pub(crate) rtc: Rtc,

    pub(crate) adb: AdbTransceiver,
}

impl Via {
    pub fn new(model: MacModel) -> Self {
        Self {
            model,
            a_out: RegisterA(0xFF),
            b_out: RegisterB(0xFF),
            a_in: model.via1_a_in(),
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
            sr: 0,
            kbdshift_in: 0,
            kbdshift_in_time: 0,
            kbdshift_out: 0,
            kbdshift_out_time: 0,

            keyboard: PlusKeyboard::default(),
            rtc: Rtc::default(),
            adb: AdbTransceiver::default(),
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
            // Timer 1 counter MSB
            0x05 => Some(self.t1cnt.msb()),

            // Timer 1 latch LSB
            0x06 => Some(self.t1latch.lsb()),
            // Timer 1 latch MSB
            0x07 => Some(self.t1latch.msb()),

            // Register B
            0x00 => {
                self.ifr.set_kbddata(false);
                self.ifr.set_kbdclock(false);

                // Lazy update ADB Int to current state
                if self.model.has_adb() {
                    self.b_in.set_adb_int(!self.adb.get_int());
                }

                Some((self.b_in.0 & !self.ddrb.0) | (self.b_out.0 & self.ddrb.0))
            }

            // Register B Data Direction
            0x02 => Some(self.ddrb.0),

            // Register A Data Direction
            0x03 => Some(self.ddra.0),

            // Shift register
            0x0A => {
                let sr = self.sr;
                self.ifr.set_kbdready(false);

                Some(sr)
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
                self.ifr.set_kbdready(false);

                self.sr = val;
                if self.acr.kbd() == ACR_SHIFT_OUT {
                    // Start shift-out
                    self.kbdshift_out = val;
                    self.kbdshift_out_time = SHIFT_DELAY;
                }
                Some(())
            }

            // Register B
            0x00 => {
                self.ifr.set_kbddata(false);
                self.ifr.set_kbdclock(false);

                self.b_out.0 = val;
                let rtcin = self.rtc.io(
                    self.b_out.rtcenb(),
                    self.b_out.rtcclk(),
                    self.b_out.rtcdata(),
                );
                self.b_in.set_rtcdata(rtcin);

                if self.model.has_adb() {
                    if let Some(b) = self.adb.io(self.b_out.adb_st0(), self.b_out.adb_st1()) {
                        self.kbdshift_in = b;
                    }
                }

                Some(())
            }

            // Register B Data Direction
            0x02 => Some(self.ddrb.0 = val),

            // Register A Data Direction
            0x03 => Some(self.ddra.0 = val),

            // Auxiliary Control Register
            0x0B => {
                let newacr = RegisterACR(val);
                if newacr.kbd() != self.acr.kbd() {
                    // Reset shifter operation
                    self.kbdshift_in_time = 0;
                    self.kbdshift_out_time = 0;
                }
                if newacr.kbd() == ACR_SHIFT_IN {
                    self.kbdshift_in_time = SHIFT_DELAY;
                } else if newacr.kbd() == ACR_SHIFT_OUT {
                    self.kbdshift_out = self.sr;
                    self.kbdshift_out_time = SHIFT_DELAY;
                }

                Some(self.acr.0 = val)
            }

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

        // ADB/Keyboard response
        if self.kbdshift_out_time > 0 {
            self.kbdshift_out_time = self.kbdshift_out_time.saturating_sub(ticks);
            if self.kbdshift_out_time == 0 {
                if !self.model.has_adb() {
                    self.kbdshift_in = self.keyboard.cmd(self.kbdshift_out)?;
                } else {
                    self.adb.data_in(self.kbdshift_out);
                }
                self.ifr.set_kbdready(true);
            }
        }
        if self.kbdshift_in_time > 0 {
            self.kbdshift_in_time = self.kbdshift_in_time.saturating_sub(ticks);
            if self.kbdshift_in_time == 0 {
                self.sr = self.kbdshift_in;
                self.ifr.set_kbdready(true);
                self.kbdshift_in = self.sr;
                self.kbdshift_in_time = SHIFT_DELAY;
            }
        }

        Ok(ticks)
    }
}

impl Debuggable for Via {
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
                "Keyboard shifter",
                vec![
                    dbgprop_byte_bin!("Shift register", self.sr),
                    dbgprop_byte_bin!("Input", self.kbdshift_in),
                    dbgprop_udec!("Input timer", self.kbdshift_in_time),
                    dbgprop_byte_bin!("Output", self.kbdshift_out),
                    dbgprop_udec!("Output timer", self.kbdshift_out_time),
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
