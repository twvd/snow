pub mod alu;
pub mod cpu;
pub mod instruction;
pub mod regs;

use num_traits::{FromBytes, PrimInt, ToBytes, WrappingAdd, WrappingShl, WrappingShr};

use crate::util::lossyinto::LossyInto;

pub type Byte = u8;
pub type Word = u16;
pub type Long = u32;

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
        + std::fmt::Display,
    Long: LossyInto<T>,
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
