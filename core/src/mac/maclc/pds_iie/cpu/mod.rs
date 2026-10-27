//! 6502-family CPU core (6502 / 65C02 / 65816), for the Apple IIe Card.
//!
//! This core is adapted from the 65816 core of the SNES emulator
//! [siena](https://github.com/twvd/siena) (`src/cpu_65816/`), by the same
//! author as Snow (twvd), who has authorised using the code here.  It was
//! fetched from siena commit
//! `023faa5160e06c24eb0d0b65949927e3f3c102c9` ("PPU: render at H=22",
//! 2024-06-12); the repository carries no license file, and the
//! provenance of the original code is noted here.
//!
//! Changes from the siena core:
//!
//! * Parameterised into a CPU family with three variants:
//!   [`Variant::M6502`] (NMOS 6502), [`Variant::M65C02`] (WDC/Rockwell
//!   65C02, the Apple IIe Card's CPU) and `Variant::W65816` (native-mode
//!   65816, kept as in siena).  The 65816 is only built with the `w65816`
//!   feature: Snow does not use it.
//! * Snow conventions: `anyhow::Result`, `serde` support for save states,
//!   [`crate::tickable::Ticks`] for cycle counts.
//! * A small bus trait ([`Bus6502`]) instead of siena's global bus, so
//!   the card can serve as the CPU's memory map.  Reads say whether they
//!   fetch an opcode, as the 6502's SYNC pin does.
//! * Suspend/halt support: the bus (the Gemini ASIC trap logic) can halt
//!   the CPU between bus accesses and resume it later.  A trapped read
//!   rewinds the current instruction so it re-executes once the bus
//!   resumes the CPU (the resumed read consumes the latched trap and the
//!   bus supplies the value the host wrote); a trapped write completes
//!   the instruction and the CPU halts before the next one.
//! * 6502/65C02 semantics that differ from the 65816: page-1 stack,
//!   8-bit registers, `$FFFA`–`$FFFF` vectors, zero-page indexed wrap,
//!   the NMOS indirect-JMP page bug, the NMOS JSR push bug, NMOS decimal
//!   flag quirks, BRK B-flag handling and the hardwired P bit 5.
//!
//! The core is validated against the SingleStepTests JSON suites in
//! `tests.rs`.

pub mod alu;
#[allow(clippy::module_inception)]
pub mod cpu;
pub mod instruction;
pub mod instruction_table;
pub mod regs;
#[cfg(test)]
mod testbus;
#[cfg(test)]
mod tests;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::tickable::Ticks;

/// Main CPU address data type (24-bit; the 6502-family variants only
/// drive the low 16 bits).
pub type Address = u32;

/// Main CPU address mask.
pub const ADDRESS_MASK: Address = 0x00FFFFFF;

/// CPU family variant.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum Variant {
    /// NMOS 6502 (illegal opcodes not modelled, decimal flags quirky).
    M6502,
    /// WDC/Rockwell 65C02 (the Apple IIe Card's CPU).
    M65C02,
    /// WDC 65816 in native mode, as in siena.
    #[cfg(feature = "w65816")]
    W65816,
}

impl Variant {
    /// Whether this is the 65816 (never, without the `w65816` feature).
    pub fn is_65816(self) -> bool {
        #[cfg(feature = "w65816")]
        return self == Self::W65816;
        #[cfg(not(feature = "w65816"))]
        false
    }
}

/// What a read is for.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ReadKind {
    /// The first byte of an instruction (the 6502's SYNC pin is high).
    Opcode,
    /// Any other read: operands, data, dummy reads, vectors.
    Data,
}

/// Memory bus as seen by the CPU.
pub trait Bus6502 {
    /// Reads a memory location.
    fn read(&self, addr: Address, kind: ReadKind) -> u8;

    /// Writes a memory location.
    fn write(&mut self, addr: Address, val: u8);

    /// Returns the state of the NMI line.
    fn get_nmi(&mut self) -> bool;

    /// Returns the state of the IRQ line.
    fn get_int(&mut self) -> bool;

    /// Address mask used for bus iterator wrapping and addressing.
    fn get_mask(&self) -> Address {
        ADDRESS_MASK
    }

    /// Runs `cycles` bus cycles of peripheral (non-CPU) time.
    ///
    /// Returns the number of cycles actually consumed.
    fn tick(&mut self, cycles: Ticks) -> Result<Ticks>;

    /// The Apple IIe Card routes NMI/IRQ vector fetches and the
    /// receiver code to its alternate firmware bank; other buses
    /// ignore this.
    fn set_int_bank(&mut self, _on: bool) {}

    /// Whether the bus has suspended (halted) the CPU.
    ///
    /// The Apple IIe Card's Gemini ASIC halts the 65C02 when it touches a
    /// trapped area and only releases it when the Mac-side host writes
    /// the resume register.  While suspended, [`cpu::Cpu::step`] does
    /// nothing.
    fn is_suspended(&self) -> bool {
        false
    }
}
