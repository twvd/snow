pub mod alu;
pub mod bus;
pub mod cpu;
pub mod disassembler;
pub mod ea;
pub mod fpu;
pub mod instruction;
pub mod pmmu;
pub mod regs;

use num_traits::{FromBytes, PrimInt, ToBytes, WrappingAdd, WrappingShl, WrappingShr};

use crate::bus::Address;
use crate::types::Long;
use crate::util::lossyinto::LossyInto;

/// Motorola 68000
pub type CpuM68000<TBus> = cpu::CpuM68k<TBus, M68000_ADDRESS_MASK, M68000, FPU_NONE, false>;
pub const M68000_ADDRESS_MASK: Address = 0x00FFFFFF;
pub const M68000_SR_MASK: u16 = 0b1010011100011111;

/// Motorola 68020 + 68881 FPU
pub type CpuM68020Fpu<TBus> = cpu::CpuM68k<TBus, M68020_ADDRESS_MASK, M68020, FPU_M68881, false>;
pub const M68020_ADDRESS_MASK: Address = 0xFFFFFFFF;
pub const M68020_SR_MASK: u16 = 0b1011011100011111;
pub const M68020_CACR_MASK: u32 = 0b1111;

/// Motorola 68020 + 68851 PMMU + 68881 FPU
pub type CpuM68020Pmmu<TBus> = cpu::CpuM68k<TBus, M68020_ADDRESS_MASK, M68020, FPU_M68881, true>;

/// Motorola 68030
pub type CpuM68030Fpu<TBus> = cpu::CpuM68k<TBus, M68030_ADDRESS_MASK, M68030, FPU_M68882, true>;
pub const M68030_ADDRESS_MASK: Address = 0xFFFFFFFF;
pub const M68030_SR_MASK: u16 = 0b1011011100011111;
pub const M68030_CACR_MASK: u32 = 0b11111100011111;

// CPU type constants for the CPU_TYPE const generic parameter of CpuM68k
// Should be replaced witb enum const generic if that ever comes to Rust..
pub type CpuM68kType = usize;
pub const M68000: CpuM68kType = 68000;
pub const M68010: CpuM68kType = 68010;
pub const M68020: CpuM68kType = 68020;
pub const M68030: CpuM68kType = 68030;

// FPU types
pub type FpuM68kType = usize;
pub const FPU_NONE: FpuM68kType = 0;
pub const FPU_M68881: FpuM68kType = 68881;
pub const FPU_M68882: FpuM68kType = 68882;

/// Trait to deal with the differently sized instructions for:
/// Byte (u8)
/// Word (u16)
/// Long (u32)
pub trait CpuSized:
    PrimInt
    + FromBytes
    + ToBytes
    + WrappingAdd
    + std::convert::Into<Long>
    + std::convert::From<u8>
    + WrappingShl
    + WrappingShr
    + std::fmt::Display
    + std::fmt::UpperHex
    + std::ops::BitOrAssign
    + std::ops::ShlAssign
    + std::ops::ShrAssign
{
    /// Expands the value in the generic to a full register's width
    fn expand(self) -> Long;

    /// Expands the value in the generic to a full register's width,
    /// with sign extension.
    fn expand_sign_extend(self) -> Long;

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
    T: PrimInt
        + FromBytes
        + ToBytes
        + WrappingAdd
        + std::convert::Into<Long>
        + std::convert::From<u8>
        + WrappingShl
        + WrappingShr
        + std::fmt::Display
        + std::fmt::UpperHex
        + std::ops::BitOrAssign
        + std::ops::ShlAssign
        + std::ops::ShrAssign,
    Long: LossyInto<T>,
    <T as ToBytes>::Bytes: AsMut<[u8]>,
    T: FromBytes<Bytes = <T as ToBytes>::Bytes>,
{
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
    fn expand_sign_extend(self) -> Long {
        let l = self.expand();
        if l & T::msb().expand() != 0 {
            match std::mem::size_of::<T>() {
                1 => l | 0xFFFFFF00,
                2 => l | 0xFFFF0000,
                4 => l,
                _ => unreachable!(),
            }
        } else {
            l
        }
    }

    #[inline(always)]
    fn chop(value: Long) -> T {
        value.lossy_into()
    }

    #[inline(always)]
    fn msb() -> Self {
        let shift = std::mem::size_of::<T>() * 8 - 1;
        T::one() << shift
    }
}

/// Temporal access order low address to high address
pub(in crate::cpu_m68k) const TORDER_LOWHIGH: usize = 0;

/// Temporal access order high address to low address
pub(in crate::cpu_m68k) const TORDER_HIGHLOW: usize = 1;
