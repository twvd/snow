use std::ops::{Mul, SubAssign};

use num::{PrimInt, Signed};

pub mod lossyinto;

pub fn take_from_accumulator<T: PrimInt + Signed + Mul<Output = T> + SubAssign>(
    accumulator: &mut T,
    max_amount: T,
) -> T {
    if *accumulator == T::zero() {
        return T::zero();
    }

    let sign = accumulator.signum();
    let available = accumulator.abs();
    let take_amount = max_amount.min(available);
    let actual_taken = sign * take_amount;

    *accumulator -= actual_taken;
    actual_taken
}
