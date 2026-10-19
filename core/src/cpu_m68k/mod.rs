/// Advances by the internal clocks of an instruction on the 68000 to 68030:
/// `m68000` on the 68000 and 68010, `m68020` or `m68030`, `all` on every one
/// of them, or `others` on those not named. A CPU that isn't named takes no
/// clocks when there is no `others`. CPU_TYPE picks the value of this CPU at
/// compile time, and only that expression is evaluated. The 68040 takes the
/// time of its instructions from its own tables instead (timing040.rs,
/// account_040_time!), so this does nothing on the 68040.
macro_rules! advance_cycles {
    ($cpu:expr, all: $c:expr $(,)?) => {
        advance_cycles!($cpu, m68000: $c, m68020: $c, m68030: $c)
    };
    ($cpu:expr, m68000: $c000:expr, others: $c:expr $(,)?) => {
        advance_cycles!($cpu, m68000: $c000, m68020: $c, m68030: $c)
    };
    ($cpu:expr, m68020: $c020:expr, others: $c:expr $(,)?) => {
        advance_cycles!($cpu, m68000: $c, m68020: $c020, m68030: $c)
    };
    ($cpu:expr, m68030: $c030:expr, others: $c:expr $(,)?) => {
        advance_cycles!($cpu, m68000: $c, m68020: $c, m68030: $c030)
    };
    ($cpu:expr, m68000: $c000:expr $(,)?) => {
        advance_cycles!($cpu, m68000: $c000, others: 0)
    };
    ($cpu:expr, m68020: $c020:expr $(,)?) => {
        advance_cycles!($cpu, m68020: $c020, others: 0)
    };
    ($cpu:expr, m68030: $c030:expr $(,)?) => {
        advance_cycles!($cpu, m68030: $c030, others: 0)
    };
    (
        $cpu:expr,
        m68000: $c000:expr,
        m68020: $c020:expr,
        m68030: $c030:expr $(,)?
    ) => {{
        let ticks: $crate::tickable::Ticks = match CPU_TYPE {
            $crate::cpu_m68k::M68000 | $crate::cpu_m68k::M68010 => $c000,
            $crate::cpu_m68k::M68020 => $c020,
            $crate::cpu_m68k::M68030 => $c030,
            _ => 0,
        };
        if ticks == 0 {
            Ok(())
        } else {
            $cpu.advance_cycles(ticks)
        }
    }};
}

/// Sets the execute stage time of the instruction of this step on the 68040,
/// which the step charges at its end (timing040.rs): a fixed time, or the time
/// in a column of the tables for an addressing mode. Compiles to nothing on
/// the other CPUs, and only evaluates its arguments on the 68040.
macro_rules! account_040_time {
    ($cpu:expr, $time:expr $(,)?) => {
        if CPU_TYPE == $crate::cpu_m68k::M68040 {
            $cpu.exec040 = $time;
        }
    };
    ($cpu:expr, $column:expr, $mode:expr $(,)?) => {
        if CPU_TYPE == $crate::cpu_m68k::M68040 {
            $cpu.exec040 = $cpu.exec040_ea(&$column, $mode);
        }
    };
}

pub mod alu;
pub mod bus;
mod bus_transaction;
pub mod cpu;
pub mod disassembler;
pub mod ea;
pub mod fpu;
pub mod instruction;
pub mod pmmu;
pub mod regs;
#[cfg(test)]
pub mod tests;
mod timing040;

use num_traits::ToBytes;

use crate::bus::Address;
use crate::types::{Long, MyUIntTraits, SignedLong};
use crate::util::lossyinto::LossyInto;

/// Motorola 68000
pub type CpuM68000<TBus> = cpu::CpuM68k<TBus, M68000_ADDRESS_MASK, M68000, FPU_NONE, false>;
pub const M68000_ADDRESS_MASK: Address = 0x00FFFFFF;
pub const M68000_SR_MASK: u16 = 0b1010011100011111;

/// Motorola 68020
pub type CpuM68020<TBus> = cpu::CpuM68k<TBus, M68020_ADDRESS_MASK, M68020, FPU_NONE, false>;

/// Motorola 68020 + 68881 FPU
pub type CpuM68020Fpu<TBus> = cpu::CpuM68k<TBus, M68020_ADDRESS_MASK, M68020, FPU_M68881, false>;

/// Motorola 68020 + 68882 FPU
pub type CpuM68020Fpu68882<TBus> =
    cpu::CpuM68k<TBus, M68020_ADDRESS_MASK, M68020, FPU_M68882, false>;
pub const M68020_ADDRESS_MASK: Address = 0xFFFFFFFF;
pub const M68020_SR_MASK: u16 = 0b1011011100011111;
pub const M68020_CACR_MASK: u32 = 0b1111;

/// Motorola 68020 + 68851 PMMU + 68881 FPU
pub type CpuM68020Pmmu<TBus> = cpu::CpuM68k<TBus, M68020_ADDRESS_MASK, M68020, FPU_M68881, true>;

/// Motorola 68030
pub type CpuM68030<TBus> = cpu::CpuM68k<TBus, M68030_ADDRESS_MASK, M68030, FPU_NONE, true>;

/// Motorola 68030 + 68882 FPU
pub type CpuM68030Fpu<TBus> = cpu::CpuM68k<TBus, M68030_ADDRESS_MASK, M68030, FPU_M68882, true>;
pub const M68030_ADDRESS_MASK: Address = 0xFFFFFFFF;
pub const M68030_SR_MASK: u16 = 0b1011011100011111;
pub const M68030_CACR_MASK: u32 = 0b11111100011111;

/// Motorola 68040 (integrated MMU + FPU)
pub type CpuM68040Fpu<TBus> = cpu::CpuM68k<TBus, M68040_ADDRESS_MASK, M68040, FPU_M68040, true>;
pub const M68040_ADDRESS_MASK: Address = 0xFFFFFFFF;
pub const M68040_SR_MASK: u16 = 0b1011011100011111;
pub const M68040_CACR_MASK: u32 = 0x80008000;

// CPU type constants for the CPU_TYPE const generic parameter of CpuM68k
// Should be replaced with enum const generic if that ever comes to Rust..
pub type CpuM68kType = usize;
pub const M68000: CpuM68kType = 68000;
pub const M68010: CpuM68kType = 68010;
pub const M68020: CpuM68kType = 68020;
pub const M68030: CpuM68kType = 68030;
pub const M68040: CpuM68kType = 68040;

// FPU types
pub type FpuM68kType = usize;
pub const FPU_NONE: FpuM68kType = 0;
pub const FPU_M68881: FpuM68kType = 68881;
pub const FPU_M68882: FpuM68kType = 68882;
pub const FPU_M68040: FpuM68kType = 68040;

/// Trait to deal with the differently sized instructions for:
/// Byte (u8)
/// Word (u16)
/// Long (u32)
pub trait CpuSized:
    MyUIntTraits
    + ToBytes
    + std::convert::Into<Long>
    + std::convert::From<u8>
    + std::fmt::Display
    + std::fmt::UpperHex
    + std::ops::BitOrAssign
    + std::ops::ShlAssign
    + std::ops::ShrAssign
{
    const BITS: usize;

    /// Expands the value in the generic to a full register's width
    fn expand(self) -> Long;

    /// Expands the value in the generic to a full register's width,
    /// with sign extension.
    fn expand_signed(self) -> SignedLong;

    /// Replaces the lower bytes of the given value for types < Long
    /// or the full value for Long.
    fn replace_in(self, value: Long) -> Long;

    /// Downcasts to T from Long, discarding excess bits.
    fn chop(value: Long) -> Self;

    /// Returns the most significant bit as one
    fn msb() -> Self;
}

impl<T> CpuSized for T
where
    T: MyUIntTraits
        + ToBytes
        + std::convert::Into<Long>
        + std::convert::From<u8>
        + std::fmt::Display
        + std::fmt::UpperHex
        + std::ops::BitOrAssign
        + std::ops::ShlAssign
        + std::ops::ShrAssign,
    Long: LossyInto<T>,
{
    const BITS: usize = std::mem::size_of::<T>() * 8;

    #[inline(always)]
    fn replace_in(self, value: Long) -> Long {
        let mask = match std::mem::size_of::<T>() {
            1 => 0xFFFFFF00,
            2 => 0xFFFF0000,
            4 => 0x00000000,
            _ => unreachable!(),
        };
        (value & mask) | self.expand()
    }

    #[inline(always)]
    fn expand(self) -> Long {
        self.into()
    }

    #[inline(always)]
    fn expand_signed(self) -> SignedLong {
        self.cast_signed().into()
    }

    #[inline(always)]
    fn chop(value: Long) -> T {
        value.lossy_into()
    }

    #[inline(always)]
    fn msb() -> Self {
        T::one() << (Self::BITS - 1)
    }
}

/// Temporal access order low address to high address
pub(in crate::cpu_m68k) const TORDER_LOWHIGH: usize = 0;

/// Temporal access order high address to low address
pub(in crate::cpu_m68k) const TORDER_HIGHLOW: usize = 1;
