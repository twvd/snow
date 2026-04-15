use super::cpu::CpuM68k;
use super::regs::RegisterSR;
use super::{CpuM68kType, CpuSized};
use crate::cpu_m68k::FpuM68kType;

use crate::bus::{Address, Bus, IrqSource};
use crate::types::{Byte, Long, MyIntTraits, Word};

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Add (a + b = c)
    pub(super) fn alu_add<T: CpuSized>(a: T, b: T, f: RegisterSR) -> (T, u8) {
        let (result, carry) = a.overflowing_add(b);
        let (_, overflow) = a.cast_signed().overflowing_add(b.cast_signed());

        let mut new_f = f;
        new_f.set_c(carry);
        new_f.set_x(carry);
        new_f.set_v(overflow);
        new_f.set_n(result & T::msb() != T::zero());
        new_f.set_z(result == T::zero());

        (result, new_f.ccr())
    }

    /// Add (a + b + x = c)
    pub(super) fn alu_add_x<T: CpuSized>(a: T, b: T, f: RegisterSR) -> (T, u8) {
        let a = a.expand();
        let b = b.expand();
        let x = if f.x() { 1 } else { 0 };
        let result: Long = a.wrapping_add(b).wrapping_add(x);

        let msb: Long = T::msb().into();
        let carry: Long = a ^ b ^ result;
        let overflow: Long = (a ^ result) & (b ^ result);

        let mut new_f = f;
        new_f.set_c((carry ^ overflow) & msb != 0);
        new_f.set_x((carry ^ overflow) & msb != 0);
        new_f.set_v(overflow & msb != 0);
        new_f.set_n(result & msb != 0);
        if T::chop(result) != T::zero() {
            new_f.set_z(false);
        }

        (T::chop(result), new_f.ccr())
    }

    /// Subtract (a - b = c)
    pub(super) fn alu_sub<T: CpuSized>(a: T, b: T, f: RegisterSR) -> (T, u8) {
        let (result, carry) = a.overflowing_sub(b);
        let (_, overflow) = a.cast_signed().overflowing_sub(b.cast_signed());

        let mut new_f = f;
        new_f.set_c(carry);
        new_f.set_x(carry);
        new_f.set_v(overflow);
        new_f.set_n(result & T::msb() != T::zero());
        new_f.set_z(result == T::zero());

        (result, new_f.ccr())
    }

    /// Subtract with extend (a - b - x = c)
    pub(super) fn alu_sub_x<T: CpuSized>(a: T, b: T, f: RegisterSR) -> (T, u8) {
        let a = a.expand();
        let b = b.expand();
        let x = if f.x() { 1 } else { 0 };
        let result: Long = a.wrapping_sub(b).wrapping_sub(x);

        let msb: Long = T::msb().into();
        let carry: Long = a ^ b ^ result;
        let overflow: Long = (a ^ result) & (b ^ a);

        let mut new_f = f;
        new_f.set_c((carry ^ overflow) & msb != 0);
        new_f.set_x((carry ^ overflow) & msb != 0);
        new_f.set_v(overflow & msb != 0);
        new_f.set_n(result & msb != 0);
        if T::chop(result) != T::zero() {
            new_f.set_z(false);
        }

        (T::chop(result), new_f.ccr())
    }

    /// Subtract with extend and BCD correction
    /// Byte only.
    pub(super) fn alu_sub_bcd(a: Byte, b: Byte, f: RegisterSR) -> (Byte, u8) {
        let x = if f.x() { 1 } else { 0 };
        let a = a as Word;
        let b = b as Word;

        let oresult: Word = a.wrapping_sub(b).wrapping_sub(x);

        let mut result = oresult;
        let mut carry = false;
        let mut overflow = false;

        if (a ^ b ^ oresult) & 0x10 != 0 {
            // Adjust low nibble
            result = result.wrapping_sub(0x06);
            carry = (!oresult & 0x80) & (result & 0x80) != 0;
            overflow = overflow || ((oresult & 0x80) & (!result & 0x80)) != 0;
        }
        if oresult & 0x100 != 0 {
            // Adjust high nibble
            let r = result;
            result = result.wrapping_sub(0x60);
            carry = true;
            overflow = overflow || ((r & 0x80) & (!result & 0x80)) != 0;
        }

        let mut new_f = f;
        new_f.set_c(carry);
        new_f.set_x(carry);
        new_f.set_v(overflow);
        new_f.set_n(result & 0x80 != 0);
        if result & 0xFF != 0 {
            // Flag untouched if result is zero
            new_f.set_z(false);
        }

        (result as Byte, new_f.ccr())
    }

    /// Add with extend and BCD correction
    /// Byte only.
    pub(super) fn alu_add_bcd(a: Byte, b: Byte, f: RegisterSR) -> (Byte, u8) {
        let x = if f.x() { 1 } else { 0 };
        let a = a as Word;
        let b = b as Word;

        let oresult: Word = a.wrapping_add(b).wrapping_add(x);

        let mut result = oresult;
        let mut carry = false;
        let mut overflow = false;

        if (a ^ b ^ oresult) & 0x10 != 0 || (oresult & 0x0F) >= 0x0A {
            // Adjust low nibble
            result = result.wrapping_add(0x06);
            overflow = overflow || ((!oresult & 0x80) & (result & 0x80)) != 0;
        }
        if result >= 0xA0 {
            // Adjust high nibble
            let r = result;
            result = result.wrapping_add(0x60);
            carry = true;
            overflow = overflow || ((!r & 0x80) & (result & 0x80)) != 0;
        }

        let mut new_f = f;
        new_f.set_c(carry);
        new_f.set_x(carry);
        new_f.set_v(overflow);
        new_f.set_n(result & 0x80 != 0);
        if result & 0xFF != 0 {
            // Flag untouched if result is zero
            new_f.set_z(false);
        }

        (result as Byte, new_f.ccr())
    }

    /// Arithmetic right shift
    pub(super) fn alu_asr<T: CpuSized>(value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        // Perform shift on a 64-bit value, since count is guaranteed to be in 0..=63.
        let value: i64 = value.expand_signed().into();

        let carry = if count == 0 {
            false
        } else {
            (value >> (count - 1)) & 1 != 0
        };

        let value = T::chop((value >> count) as Long);

        f.set_c(carry);
        f.set_v(false);
        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        if count != 0 {
            f.set_x(carry);
        }

        (value, f.ccr())
    }

    /// Arithmetic left shift
    pub(super) fn alu_asl<T: CpuSized>(value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        // Perform shift on a 64-bit value, since count is guaranteed to be in 0..=63.
        let zext_value: u64 = value.expand().into();
        let sext_value: i64 = value.expand_signed().into();

        // Detect if the most significant bit changes at any time during the shift operation.
        let initial_upper_and_sign = sext_value >> (T::BITS - 1);
        let new_upper_and_sign = (sext_value << count) >> (T::BITS - 1);
        let overflow = initial_upper_and_sign != new_upper_and_sign;

        // Compute carry flag. Use the zero-extended value here to ensure C is correct if
        // count == 0.
        let value = zext_value << count;
        let carry = value & (1u64 << T::BITS) != 0;

        let value = T::chop(value as Long);

        f.set_c(carry);
        f.set_v(overflow);
        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        if count != 0 {
            f.set_x(carry);
        }
        (value, f.ccr())
    }

    /// Logical left shift
    pub(super) fn alu_lsl<T: CpuSized>(value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        // Perform shift on a 64-bit value, since count is guaranteed to be in 0..=63.
        let value: u64 = value.expand().into();

        let value = value << count;
        let carry = value & (1u64 << T::BITS) != 0;

        let value = T::chop(value as Long);

        f.set_c(carry);
        f.set_v(false);
        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        if count != 0 {
            f.set_x(carry);
        }
        (value, f.ccr())
    }

    /// Logical right shift
    pub(super) fn alu_lsr<T: CpuSized>(value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        // Perform shift on a 64-bit value, since count is guaranteed to be in 0..=63.
        let value: u64 = value.expand().into();

        let carry = if count == 0 {
            false
        } else {
            (value >> (count - 1)) & 1 != 0
        };

        let value = T::chop((value >> count) as Long);

        f.set_c(carry);
        f.set_v(false);
        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        if count != 0 {
            f.set_x(carry);
        }
        (value, f.ccr())
    }

    /// Rotate left
    pub(super) fn alu_rol<T: CpuSized>(mut value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        // For shift count 0, carry is cleared
        f.set_c(false);

        for _ in 0..count {
            f.set_c(value & T::msb() != T::zero());

            value <<= T::one();
            if f.c() {
                value |= T::one();
            }
        }

        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        f.set_v(false);
        (value, f.ccr())
    }

    /// Rotate right
    pub(super) fn alu_ror<T: CpuSized>(mut value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        // For shift count 0, carry is cleared
        f.set_c(false);

        for _ in 0..count {
            f.set_c(value & T::one() != T::zero());

            value >>= T::one();
            if f.c() {
                value |= T::msb();
            }
        }

        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        f.set_v(false);
        (value, f.ccr())
    }

    /// Rotate left with extend
    pub(super) fn alu_roxl<T: CpuSized>(mut value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        for _ in 0..count {
            let x = f.x();
            f.set_x(value & T::msb() != T::zero());

            value <<= T::one();
            if x {
                value |= T::one();
            }
        }

        f.set_c(f.x());
        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        f.set_v(false);
        (value, f.ccr())
    }

    /// Rotate right with extend
    pub(super) fn alu_roxr<T: CpuSized>(mut value: T, count: u8, mut f: RegisterSR) -> (T, u8) {
        for _ in 0..count {
            let x = f.x();
            f.set_x(value & T::one() != T::zero());

            value >>= T::one();
            if x {
                value |= T::msb();
            }
        }

        f.set_c(f.x());
        f.set_z(value == T::zero());
        f.set_n(value & T::msb() != T::zero());
        f.set_v(false);
        (value, f.ccr())
    }
}
