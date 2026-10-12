//! Common 6522 VIA core
//!
//! Implements the register file, timers and interrupt logic. The machine
//! specific wiring of the ports and control lines is defined by a wrapper
//! through [`ViaWiring`].

use crate::bus::Address;
use crate::debuggable::{DebuggableProperties, DebuggableProperty, DebuggablePropertyValue};
use crate::tickable::Ticks;
use crate::types::{Byte, Field16};
use crate::{dbgprop_bool, dbgprop_byte_bin, dbgprop_group, dbgprop_word};

use anyhow::Result;
use proc_bitfield::bitfield;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// IFR/IER bit: CA2 active edge
pub const IRQ_CA2: u8 = 1 << 0;
/// IFR/IER bit: CA1 active edge
pub const IRQ_CA1: u8 = 1 << 1;
/// IFR/IER bit: shift register completed 8 shifts
pub const IRQ_SR: u8 = 1 << 2;
/// IFR/IER bit: CB2 active edge
pub const IRQ_CB2: u8 = 1 << 3;
/// IFR/IER bit: CB1 active edge
pub const IRQ_CB1: u8 = 1 << 4;
/// IFR/IER bit: Timer 2 time-out
pub const IRQ_T2: u8 = 1 << 5;
/// IFR/IER bit: Timer 1 time-out
pub const IRQ_T1: u8 = 1 << 6;

const PB7: u8 = 1 << 7;

/// VIA register select (RS3-RS0)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViaReg {
    /// Register B
    B,
    /// Register A
    A,
    /// Register B Data Direction
    DDRB,
    /// Register A Data Direction
    DDRA,
    /// Timer 1 counter LSB
    T1CL,
    /// Timer 1 counter MSB
    T1CH,
    /// Timer 1 latch LSB
    T1LL,
    /// Timer 1 latch MSB
    T1LH,
    /// Timer 2 counter LSB
    T2CL,
    /// Timer 2 counter MSB
    T2CH,
    /// Shift register
    SR,
    /// Auxiliary Control Register
    ACR,
    /// Peripheral Control Register
    PCR,
    /// Interrupt Flag Register
    IFR,
    /// Interrupt Enable Register
    IER,
    /// Register A (no handshake)
    ANH,
}

impl ViaReg {
    /// Decodes the register from a bus address. The register select lines
    /// are connected to A9-A12 on all Macintosh models.
    pub fn from_addr(addr: Address) -> Self {
        match (addr >> 9) & 0x0F {
            0x00 => Self::B,
            0x01 => Self::A,
            0x02 => Self::DDRB,
            0x03 => Self::DDRA,
            0x04 => Self::T1CL,
            0x05 => Self::T1CH,
            0x06 => Self::T1LL,
            0x07 => Self::T1LH,
            0x08 => Self::T2CL,
            0x09 => Self::T2CH,
            0x0A => Self::SR,
            0x0B => Self::ACR,
            0x0C => Self::PCR,
            0x0D => Self::IFR,
            0x0E => Self::IER,
            0x0F => Self::ANH,
            _ => unreachable!(),
        }
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

        /// Shift register mode
        pub sr_mode: u8 @ 2..=4,

        /// Timer T2 mode (true = count pulses on PB6)
        pub t2: bool @ 5,

        /// Timer T1 free-running mode
        pub t1_freerun: bool @ 6,

        /// Timer T1 output enable (on PB7)
        pub t1_output_en: bool @ 7,
    }
}

bitfield! {
    /// VIA Peripheral Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterPCR(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// CA1 active edge
        pub ca1: bool @ 0,

        /// CA2 control
        pub ca2: u8 @ 1..=3,

        /// CB1 active edge
        pub cb1: bool @ 4,

        /// CB2 control
        pub cb2: u8 @ 5..=7,
    }
}

/// When the PCR configures CA2 (bits 3-1) or CB2 (bits 7-5) in mode %001 or
/// %011, the line is in "independent interrupt" mode: its interrupt flag is
/// no longer cleared by an access to register A (CA2) or B (CB2), only by
/// writing the IFR.
///
/// See Synertek SY6522 Applications Information AN5, "Corrections to earlier
/// data sheets", note 1.
impl RegisterPCR {
    pub fn ca2_independent(&self) -> bool {
        self.0 & 0x0A == 0x02
    }

    pub fn cb2_independent(&self) -> bool {
        self.0 & 0xA0 == 0x20
    }
}

/// VIA control line inputs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViaLine {
    CA1,
    CA2,
    CB1,
    CB2,
}

impl ViaLine {
    /// The interrupt flag of this line
    pub const fn irq(self) -> u8 {
        match self {
            Self::CA1 => IRQ_CA1,
            Self::CA2 => IRQ_CA2,
            Self::CB1 => IRQ_CB1,
            Self::CB2 => IRQ_CB2,
        }
    }
}

/// Level to drive on a control line
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineState {
    High,
    Low,
    /// Momentarily drives the opposite of the current level
    Pulse,
}

impl From<bool> for LineState {
    fn from(high: bool) -> Self {
        if high { Self::High } else { Self::Low }
    }
}

/// A register of which the bit assignments are machine specific
pub trait ViaBits: Copy + From<u8> + Into<u8> + Serialize + DeserializeOwned {}
impl<T> ViaBits for T where T: Copy + From<u8> + Into<u8> + Serialize + DeserializeOwned {}

/// Describes how a VIA is wired up in a specific machine.
pub trait ViaWiring {
    /// Bit assignments of register A
    type RegisterA: ViaBits;

    /// Bit assignments of register B
    type RegisterB: ViaBits;

    /// Bit assignments of the interrupt flag/enable registers
    type RegisterIRQ: ViaBits;

    /// Whether an access to register B clears the CB1/CB2 interrupt flags
    const B_CLEARS_IRQ: bool = true;

    /// Whether the Timer 1 output drives PB7 if enabled through the ACR
    const T1_PB7_OUTPUT: bool = true;
}

/// Machine-independent part of a 6522 Versatile Interface Adapter
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct ViaCommon<W: ViaWiring> {
    /// Register A, outputs
    pub a_out: W::RegisterA,

    /// Register A, inputs
    pub a_in: W::RegisterA,

    /// Data Direction Register A
    pub ddra: W::RegisterA,

    /// Register B, outputs
    pub b_out: W::RegisterB,

    /// Register B, inputs
    pub b_in: W::RegisterB,

    /// Data Direction Register B
    pub ddrb: W::RegisterB,

    /// Interrupt Enable Register
    pub ier: W::RegisterIRQ,

    /// Interrupt Flag Register
    pub ifr: W::RegisterIRQ,

    /// Peripheral Control Register
    pub pcr: RegisterPCR,

    /// Auxiliary Control Register
    pub acr: RegisterACR,

    /// Shift register
    pub sr: u8,

    /// Timer 1 Counter
    pub t1cnt: Field16,

    /// Timer 1 latch
    pub t1latch: Field16,

    t1_enable: bool,

    /// T1 output state is separate from the actual PB7 output
    t1_output: bool,

    /// Timer 2 Counter
    pub t2cnt: Field16,

    /// Timer 2 latch
    pub t2latch: Field16,

    t2_enable: bool,

    /// Levels of the control lines, by interrupt flag (set = high)
    lines: u8,
}

impl<W: ViaWiring> Default for ViaCommon<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W: ViaWiring> ViaCommon<W> {
    /// Creates a VIA in reset state; all pins are inputs and read high.
    pub fn new() -> Self {
        Self {
            a_out: 0.into(),
            a_in: 0xFF.into(),
            ddra: 0.into(),
            b_out: 0.into(),
            b_in: 0xFF.into(),
            ddrb: 0.into(),
            ier: 0.into(),
            ifr: 0.into(),
            pcr: RegisterPCR(0),
            acr: RegisterACR(0),
            sr: 0,

            t1cnt: Field16(0),
            t1latch: Field16(0),
            t1_enable: false,
            t1_output: false,

            t2cnt: Field16(0),
            t2latch: Field16(0),
            t2_enable: false,

            lines: IRQ_CA1 | IRQ_CA2 | IRQ_CB1 | IRQ_CB2,
        }
    }

    /// Resets the registers. The timers and shift register are not affected.
    pub fn reset(&mut self) {
        self.a_out = 0.into();
        self.ddra = 0.into();
        self.b_out = 0.into();
        self.ddrb = 0.into();
        self.ier = 0.into();
        self.ifr = 0.into();
        self.pcr = RegisterPCR(0);
        self.acr = RegisterACR(0);
    }

    /// Sets the given interrupt flag(s)
    pub fn set_irq(&mut self, mask: u8) {
        self.ifr = (self.ifr.into() | (mask & 0x7F)).into();
    }

    /// Clears the given interrupt flag(s)
    pub fn clear_irq(&mut self, mask: u8) {
        self.ifr = (self.ifr.into() & !mask).into();
    }

    /// Tests if a transition to the given level is the active edge of a line,
    /// as selected through the PCR
    fn is_active_edge(&self, line: ViaLine, high: bool) -> bool {
        let pcr = self.pcr.0;
        let (positive, input) = match line {
            ViaLine::CA1 => (pcr & 0x01 != 0, true),
            ViaLine::CA2 => (pcr & 0x04 != 0, pcr & 0x08 == 0),
            ViaLine::CB1 => (pcr & 0x10 != 0, true),
            ViaLine::CB2 => (pcr & 0x40 != 0, pcr & 0x80 == 0),
        };
        input && positive == high
    }

    fn set_line_level(&mut self, line: ViaLine, high: bool) {
        let irq = line.irq();
        if (self.lines & irq != 0) == high {
            return;
        }
        self.lines ^= irq;

        if self.is_active_edge(line, high) {
            self.set_irq(irq);
        }
    }

    /// Drives a control line input
    pub fn set_line(&mut self, line: ViaLine, state: LineState) {
        match state {
            LineState::High => self.set_line_level(line, true),
            LineState::Low => self.set_line_level(line, false),
            LineState::Pulse => {
                let high = self.lines & line.irq() != 0;
                self.set_line_level(line, !high);
                self.set_line_level(line, high);
            }
        }
    }

    /// Register A access with handshake. The Mac uses the register without
    /// handshake (vBufA), which leaves the flags alone.
    fn clear_irq_a(&mut self) {
        self.clear_irq(IRQ_CA1);
        if !self.pcr.ca2_independent() {
            self.clear_irq(IRQ_CA2);
        }
    }

    fn clear_irq_b(&mut self) {
        if !W::B_CLEARS_IRQ {
            return;
        }
        self.clear_irq(IRQ_CB1);
        if !self.pcr.cb2_independent() {
            self.clear_irq(IRQ_CB2);
        }
    }

    /// Reads a register
    pub fn read(&mut self, reg: ViaReg) -> Byte {
        match reg {
            ViaReg::T2CL => {
                self.clear_irq(IRQ_T2);
                self.t2cnt.lsb()
            }
            ViaReg::T2CH => self.t2cnt.msb(),

            ViaReg::T1CL => {
                self.clear_irq(IRQ_T1);
                self.t1cnt.lsb()
            }
            ViaReg::T1CH => self.t1cnt.msb(),

            ViaReg::T1LL => self.t1latch.lsb(),
            ViaReg::T1LH => self.t1latch.msb(),

            ViaReg::B => {
                self.clear_irq_b();

                let (b_in, b_out, ddrb) = (self.b_in.into(), self.b_out.into(), self.ddrb.into());
                (b_in & !ddrb) | (b_out & ddrb)
            }
            ViaReg::DDRB => self.ddrb.into(),
            ViaReg::DDRA => self.ddra.into(),

            ViaReg::SR => {
                self.clear_irq(IRQ_SR);
                self.sr
            }

            ViaReg::ACR => self.acr.0,
            ViaReg::PCR => self.pcr.0,

            ViaReg::IFR => {
                let (ifr, ier) = (self.ifr.into(), self.ier.into());
                let mut val = ifr & 0x7F;
                if ifr & ier != 0 {
                    val |= 0x80;
                }
                val
            }
            ViaReg::IER => self.ier.into() | 0x80,

            ViaReg::A | ViaReg::ANH => {
                if reg == ViaReg::A {
                    self.clear_irq_a();
                }

                let (a_in, a_out, ddra) = (self.a_in.into(), self.a_out.into(), self.ddra.into());
                (a_in & !ddra) | (a_out & ddra)
            }
        }
    }

    /// Writes a register
    pub fn write(&mut self, reg: ViaReg, val: Byte) {
        match reg {
            ViaReg::T2CL => self.t2latch.set_lsb(val),
            ViaReg::T2CH => {
                self.t2latch.set_msb(val);
                self.clear_irq(IRQ_T2);

                // Start timer
                self.t2_enable = true;
                self.t2cnt = self.t2latch;
            }

            ViaReg::T1CL | ViaReg::T1LL => self.t1latch.set_lsb(val),
            ViaReg::T1LH => {
                self.t1latch.set_msb(val);
                self.clear_irq(IRQ_T1);
            }
            ViaReg::T1CH => {
                self.t1latch.set_msb(val);
                self.t1cnt.0 = self.t1latch.0;
                self.clear_irq(IRQ_T1);

                // Start timer
                self.t1_enable = true;
                self.t1_output = false;
                self.update_pb7();
            }

            ViaReg::SR => {
                self.clear_irq(IRQ_SR);
                self.sr = val;
            }

            ViaReg::B => {
                self.clear_irq_b();
                self.b_out = val.into();
            }
            ViaReg::DDRB => self.ddrb = val.into(),
            ViaReg::DDRA => self.ddra = val.into(),

            ViaReg::ACR => self.acr.0 = val,
            ViaReg::PCR => self.pcr.0 = val,

            ViaReg::IFR => self.clear_irq(val & 0x7F),
            ViaReg::IER => {
                let ier = self.ier.into();
                self.ier = if val & 0x80 != 0 {
                    // Enable
                    ier | (val & 0x7F)
                } else {
                    // Disable
                    ier & !(val & 0x7F)
                }
                .into();
            }

            ViaReg::A | ViaReg::ANH => {
                if reg == ViaReg::A {
                    self.clear_irq_a();
                }
                self.a_out = val.into();
            }
        }
    }

    /// Advances the timers by the given amount of VIA clock cycles
    ///
    /// TODO a real 6522 times out N+1.5 cycles after loading N and has a
    /// period of N+2 cycles in free-running mode.
    pub fn tick_timers(&mut self, ticks: Ticks) -> Result<()> {
        // Timer 1
        let t1ovf;
        (self.t1cnt.0, t1ovf) = self.t1cnt.0.overflowing_sub(ticks.try_into()?);

        if t1ovf && self.t1_enable {
            self.set_irq(IRQ_T1);
            if !self.acr.t1_freerun() {
                // Single shot mode
                self.t1_enable = false;
                self.t1_output = true;
            } else {
                // Free running mode
                self.t1cnt.0 = self.t1latch.0;
                self.t1_output = !self.t1_output;
            }
            self.update_pb7();
        }

        // Timer 2
        let t2ovf;
        (self.t2cnt.0, t2ovf) = self.t2cnt.0.overflowing_sub(ticks.try_into()?);

        if t2ovf && self.t2_enable {
            self.t2_enable = false;
            self.set_irq(IRQ_T2);
        }

        Ok(())
    }

    fn update_pb7(&mut self) {
        if W::T1_PB7_OUTPUT && self.acr.t1_output_en() {
            let b_out = self.b_out.into() & !PB7;
            self.b_out = (b_out | if self.t1_output { PB7 } else { 0 }).into();
        }
    }

    /// Debug properties for the ports and timers
    pub fn debug_properties_ports(&self) -> DebuggableProperties {
        vec![
            dbgprop_group!(
                "Register A",
                vec![
                    dbgprop_byte_bin!("DDRA", self.ddra.into()),
                    dbgprop_byte_bin!("Inputs", self.a_in.into()),
                    dbgprop_byte_bin!("Outputs", self.a_out.into()),
                ]
            ),
            dbgprop_group!(
                "Register B",
                vec![
                    dbgprop_byte_bin!("DDRB", self.ddrb.into()),
                    dbgprop_byte_bin!("Inputs", self.b_in.into()),
                    dbgprop_byte_bin!("Outputs", self.b_out.into()),
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
        ]
    }

    /// Debug properties for the control and interrupt registers
    pub fn debug_properties_control(&self) -> DebuggableProperties {
        vec![
            dbgprop_byte_bin!("Interrupt Enable (IER)", self.ier.into()),
            dbgprop_byte_bin!("Interrupt Flags (IFR)", self.ifr.into()),
            dbgprop_byte_bin!("Peripheral Control (PCR)", self.pcr.0),
            dbgprop_byte_bin!("Auxiliary Control (ACR)", self.acr.0),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestWiring;
    impl ViaWiring for TestWiring {
        type RegisterA = u8;
        type RegisterB = u8;
        type RegisterIRQ = u8;
    }

    struct NoPb7Wiring;
    impl ViaWiring for NoPb7Wiring {
        type RegisterA = u8;
        type RegisterB = u8;
        type RegisterIRQ = u8;

        const B_CLEARS_IRQ: bool = false;
        const T1_PB7_OUTPUT: bool = false;
    }

    type TestVia = ViaCommon<TestWiring>;

    #[test]
    fn reg_decode() {
        assert_eq!(ViaReg::from_addr(0xEFFFFE), ViaReg::ANH);
        assert_eq!(ViaReg::from_addr(0xEFFBFE), ViaReg::IFR);
        assert_eq!(ViaReg::from_addr(0x0000), ViaReg::B);
        assert_eq!(ViaReg::from_addr(0x1400), ViaReg::SR);
    }

    #[test]
    fn port_ddr_mix() {
        let mut via = TestVia::new();
        via.a_in = 0b1010_1010;
        via.write(ViaReg::A, 0b0000_1111);
        via.write(ViaReg::DDRA, 0b0011_1100);
        assert_eq!(via.read(ViaReg::A), 0b1000_1110);
        assert_eq!(via.read(ViaReg::ANH), 0b1000_1110);
        assert_eq!(via.read(ViaReg::DDRA), 0b0011_1100);

        via.b_in = 0b0101_0101;
        via.write(ViaReg::B, 0b1111_0000);
        via.write(ViaReg::DDRB, 0b1100_0011);
        assert_eq!(via.read(ViaReg::B), 0b1101_0100);
    }

    #[test]
    fn port_access_clears_irq() {
        let mut via = TestVia::new();
        via.set_irq(0x7F);
        via.read(ViaReg::A);
        assert_eq!(via.ifr, 0x7F & !(IRQ_CA1 | IRQ_CA2));
        via.write(ViaReg::B, 0);
        assert_eq!(via.ifr, IRQ_SR | IRQ_T2 | IRQ_T1);

        via.set_irq(0x7F);
        via.write(ViaReg::A, 0);
        via.read(ViaReg::B);
        assert_eq!(via.ifr, IRQ_SR | IRQ_T2 | IRQ_T1);

        let mut via = ViaCommon::<NoPb7Wiring>::new();
        via.set_irq(0x7F);
        via.read(ViaReg::B);
        via.write(ViaReg::B, 0);
        assert_eq!(via.ifr, 0x7F);
    }

    #[test]
    fn reg_a_no_handshake_keeps_irq_flags() {
        let mut via = TestVia::new();
        via.set_irq(0x7F);
        via.read(ViaReg::ANH);
        via.write(ViaReg::ANH, 0);
        assert_eq!(via.ifr, 0x7F);
    }

    #[test]
    fn independent_ca2_cb2_keep_irq_flags() {
        let mut via = TestVia::new();

        // Independent interrupt inputs, negative edge
        via.write(ViaReg::PCR, 0x22);
        via.set_irq(0x7F);
        via.read(ViaReg::A);
        via.read(ViaReg::B);
        assert_eq!(via.ifr, 0x7F & !(IRQ_CA1 | IRQ_CB1));

        // Positive edge
        via.write(ViaReg::PCR, 0x66);
        via.set_irq(0x7F);
        via.write(ViaReg::A, 0);
        via.write(ViaReg::B, 0);
        assert_eq!(via.ifr, 0x7F & !(IRQ_CA1 | IRQ_CB1));

        // Handshake outputs are not independent
        via.write(ViaReg::PCR, 0xAA);
        via.read(ViaReg::A);
        via.read(ViaReg::B);
        assert_eq!(via.ifr, IRQ_SR | IRQ_T2 | IRQ_T1);
    }

    #[test]
    fn ifr_ier() {
        let mut via = TestVia::new();
        assert_eq!(via.read(ViaReg::IER), 0x80);
        assert_eq!(via.read(ViaReg::IFR), 0);

        // Flag without enable does not set the summary bit
        via.set_irq(IRQ_CA1 | IRQ_T1);
        assert_eq!(via.read(ViaReg::IFR), IRQ_CA1 | IRQ_T1);

        via.write(ViaReg::IER, 0x80 | IRQ_CA1 | IRQ_SR);
        assert_eq!(via.read(ViaReg::IER), 0x80 | IRQ_CA1 | IRQ_SR);
        assert_eq!(via.read(ViaReg::IFR), 0x80 | IRQ_CA1 | IRQ_T1);

        via.write(ViaReg::IER, IRQ_CA1);
        assert_eq!(via.read(ViaReg::IER), 0x80 | IRQ_SR);
        assert_eq!(via.read(ViaReg::IFR), IRQ_CA1 | IRQ_T1);

        // Writing ones clears flags, bit 7 is ignored
        via.write(ViaReg::IFR, 0x80 | IRQ_CA1);
        assert_eq!(via.read(ViaReg::IFR), IRQ_T1);
    }

    #[test]
    fn t1_one_shot() {
        let mut via = TestVia::new();
        via.write(ViaReg::T1CL, 0x10);
        assert_eq!(via.t1cnt.0, 0);
        via.write(ViaReg::T1CH, 0x00);
        assert_eq!(via.t1cnt.0, 0x0010);
        assert_eq!(via.read(ViaReg::T1LL), 0x10);
        assert_eq!(via.read(ViaReg::T1LH), 0x00);

        via.tick_timers(0x10).unwrap();
        assert_eq!(via.ifr, 0);
        via.tick_timers(1).unwrap();
        assert_eq!(via.ifr, IRQ_T1);

        // Reading the counter LSB clears the flag
        via.read(ViaReg::T1CH);
        assert_eq!(via.ifr, IRQ_T1);
        via.read(ViaReg::T1CL);
        assert_eq!(via.ifr, 0);

        // Counter keeps running but does not fire again
        via.tick_timers(0xFFFF).unwrap();
        via.tick_timers(0xFFFF).unwrap();
        assert_eq!(via.ifr, 0);
    }

    #[test]
    fn t1_free_run() {
        let mut via = TestVia::new();
        via.write(ViaReg::ACR, 0x40);
        via.write(ViaReg::T1CL, 0x04);
        via.write(ViaReg::T1CH, 0x00);

        via.tick_timers(5).unwrap();
        assert_eq!(via.ifr, IRQ_T1);
        assert_eq!(via.t1cnt.0, 0x0004);

        // Writing the latch MSB clears the flag without reloading the counter
        via.tick_timers(1).unwrap();
        via.write(ViaReg::T1LH, 0x01);
        assert_eq!(via.ifr, 0);
        assert_eq!(via.t1cnt.0, 0x0003);

        via.tick_timers(4).unwrap();
        assert_eq!(via.ifr, IRQ_T1);
        assert_eq!(via.t1cnt.0, 0x0104);
    }

    #[test]
    fn t1_pb7_output() {
        let mut via = TestVia::new();
        via.write(ViaReg::B, 0xFF);
        via.write(ViaReg::ACR, 0xC0);
        via.write(ViaReg::T1CL, 0x01);
        via.write(ViaReg::T1CH, 0x00);
        assert_eq!(via.b_out, 0x7F);
        via.tick_timers(2).unwrap();
        assert_eq!(via.b_out, 0xFF);
        via.tick_timers(2).unwrap();
        assert_eq!(via.b_out, 0x7F);

        // Output not enabled in ACR
        let mut via = TestVia::new();
        via.write(ViaReg::B, 0xFF);
        via.write(ViaReg::ACR, 0x40);
        via.write(ViaReg::T1CL, 0x01);
        via.write(ViaReg::T1CH, 0x00);
        assert_eq!(via.b_out, 0xFF);

        // Output not wired
        let mut via = ViaCommon::<NoPb7Wiring>::new();
        via.write(ViaReg::B, 0xFF);
        via.write(ViaReg::ACR, 0xC0);
        via.write(ViaReg::T1CL, 0x01);
        via.write(ViaReg::T1CH, 0x00);
        assert_eq!(via.b_out, 0xFF);
        via.tick_timers(2).unwrap();
        assert_eq!(via.b_out, 0xFF);
    }

    #[test]
    fn t2_one_shot() {
        let mut via = TestVia::new();
        via.write(ViaReg::T2CL, 0x02);
        via.write(ViaReg::T2CH, 0x01);
        assert_eq!(via.t2cnt.0, 0x0102);
        assert_eq!(via.read(ViaReg::T2CH), 0x01);

        via.tick_timers(0x0102).unwrap();
        assert_eq!(via.ifr, 0);
        via.tick_timers(1).unwrap();
        assert_eq!(via.ifr, IRQ_T2);

        via.read(ViaReg::T2CL);
        assert_eq!(via.ifr, 0);
        via.tick_timers(0xFFFF).unwrap();
        via.tick_timers(0xFFFF).unwrap();
        assert_eq!(via.ifr, 0);

        // Re-arming clears a pending flag
        via.set_irq(IRQ_T2);
        via.write(ViaReg::T2CH, 0x00);
        assert_eq!(via.ifr, 0);
    }

    #[test]
    fn shift_register() {
        let mut via = TestVia::new();
        via.set_irq(IRQ_SR);
        via.write(ViaReg::SR, 0xA5);
        assert_eq!(via.ifr, 0);

        via.sr = 0x5A;
        via.set_irq(IRQ_SR);
        assert_eq!(via.read(ViaReg::SR), 0x5A);
        assert_eq!(via.ifr, 0);
    }

    #[test]
    fn line_edge() {
        let mut via = TestVia::new();

        // Negative edge
        via.set_line(ViaLine::CA1, LineState::High);
        assert_eq!(via.ifr, 0);
        via.set_line(ViaLine::CA1, LineState::Low);
        assert_eq!(via.ifr, IRQ_CA1);

        // No edge
        via.write(ViaReg::IFR, IRQ_CA1);
        via.set_line(ViaLine::CA1, LineState::Low);
        assert_eq!(via.ifr, 0);
        via.set_line(ViaLine::CA1, LineState::High);
        assert_eq!(via.ifr, 0);

        // Positive edge
        via.write(ViaReg::PCR, 0x01);
        via.set_line(ViaLine::CA1, LineState::Low);
        assert_eq!(via.ifr, 0);
        via.set_line(ViaLine::CA1, LineState::High);
        assert_eq!(via.ifr, IRQ_CA1);
    }

    #[test]
    fn line_edge_pcr() {
        for (line, shift) in [(ViaLine::CA2, 1), (ViaLine::CB2, 5)] {
            let mut via = TestVia::new();

            // Positive edge, also in independent mode
            for mode in [0b010, 0b011] {
                via.write(ViaReg::PCR, mode << shift);
                via.set_line(line, LineState::Low);
                assert_eq!(via.ifr, 0);
                via.set_line(line, LineState::High);
                assert_eq!(via.ifr, line.irq());
                via.write(ViaReg::IFR, line.irq());
            }

            // Output
            via.write(ViaReg::PCR, 0b100 << shift);
            via.set_line(line, LineState::Pulse);
            assert_eq!(via.ifr, 0);
        }

        let mut via = TestVia::new();
        via.write(ViaReg::PCR, 0x10);
        via.set_line(ViaLine::CB1, LineState::Low);
        assert_eq!(via.ifr, 0);
        via.set_line(ViaLine::CB1, LineState::High);
        assert_eq!(via.ifr, IRQ_CB1);
    }

    #[test]
    fn line_pulse() {
        for pcr in [0x00, 0x01] {
            let mut via = TestVia::new();
            via.write(ViaReg::PCR, pcr);
            via.set_line(ViaLine::CA1, LineState::Pulse);
            assert_eq!(via.ifr, IRQ_CA1);

            via.write(ViaReg::IFR, IRQ_CA1);
            via.set_line(ViaLine::CA1, LineState::Pulse);
            assert_eq!(via.ifr, IRQ_CA1);
        }
    }
}
