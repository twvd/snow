pub mod lossyinto;

use std::ops::{Mul, SubAssign};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use num::{PrimInt, Signed};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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

/// Serde default helper for Instant::now()
pub fn instant_now() -> Instant {
    Instant::now()
}

/// serialize_with helper for Arc::RwLock<T>
pub fn serialize_arc_rwlock<S, T>(val: &Arc<RwLock<T>>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    val.read().unwrap().serialize(s)
}

/// deserialize_with helper for Arc::RwLock<T>
pub fn deserialize_arc_rwlock<'de, D, T>(d: D) -> Result<Arc<RwLock<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Arc::new(RwLock::new(T::deserialize(d)?)))
}
