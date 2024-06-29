use super::cpu::CpuM68k;
use super::regs::RegisterSR;
use super::{CpuSized, Long};

use crate::bus::{Address, Bus};

impl<TBus> CpuM68k<TBus>
where
    TBus: Bus<Address>,
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
}
