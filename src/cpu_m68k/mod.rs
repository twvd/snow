pub mod cpu;
pub mod instruction;
pub mod regs;

use num_traits::{FromBytes, PrimInt, ToBytes, WrappingAdd, WrappingShl};

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
}

impl<T> CpuSized for T
where
    T: PrimInt
        + FromBytes
        + ToBytes
        + WrappingAdd
        + std::convert::Into<Long>
        + std::convert::From<u8>
        + WrappingShl,
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
        match std::mem::size_of::<T>() {
            1 => self.expand() | 0xFFFFFF00,
            2 => self.expand() | 0xFFFF0000,
            4 => self.expand(),
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    fn chop(value: Long) -> T {
        value.lossy_into()
    }
}
