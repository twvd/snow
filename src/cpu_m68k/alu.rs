use super::cpu::CpuM68k;
use super::regs::RegisterSR;
use super::{Byte, CpuSized, Long, Word};

use crate::bus::{Address, Bus};

impl<TBus> CpuM68k<TBus>
where
    TBus: Bus<Address, u8>,
{
    /// Add (a + b = c)
    pub(super) fn alu_add<T: CpuSized>(a: T, b: T, f: RegisterSR) -> (T, u8) {
        let a = a.expand();
        let b = b.expand();
        let result: Long = a.wrapping_add(b);

        let msb: Long = T::msb().into();
        let carry: Long = a ^ b ^ result;
        let overflow: Long = (a ^ result) & (b ^ result);

        let mut new_f = f;
        new_f.set_c((carry ^ overflow) & msb != 0);
        new_f.set_x((carry ^ overflow) & msb != 0);
        new_f.set_v(overflow & msb != 0);
        new_f.set_n(result & msb != 0);
        new_f.set_z(T::chop(result) == T::zero());

        (T::chop(result), new_f.ccr())
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
        let a = a.expand();
        let b = b.expand();
        let result: Long = a.wrapping_sub(b);

        let msb: Long = T::msb().into();
        let carry: Long = a ^ b ^ result;
        let overflow: Long = (a ^ result) & (b ^ a);

        let mut new_f = f;
        new_f.set_c((carry ^ overflow) & msb != 0);
        new_f.set_x((carry ^ overflow) & msb != 0);
        new_f.set_v(overflow & msb != 0);
        new_f.set_n(result & msb != 0);
        new_f.set_z(T::chop(result) == T::zero());

        (T::chop(result), new_f.ccr())
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
}
