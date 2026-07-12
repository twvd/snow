use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Representation of X amount of ticks (T-cycles)
/// of the main system clock.
pub type Ticks = u64;

pub trait Tickable<TContext = ()> {
    fn tick(&mut self, ticks: Ticks, ctx: TContext) -> Result<Ticks>;
}

/// Converts ticks from one clock frequency (A) to another (B).
///
/// Frequency A can be decided at runtime, while frequency B must be
/// specified as a const generic.
///
/// Rational arithmetic is used to prevent errors from accumulating.
/// The struct holds the numerator `N` in `N / B_FREQ`, where `B_FREQ`
/// is the frequency of clock B.
#[derive(Default, Serialize, Deserialize)]
pub struct TickConverter<const B_FREQ: Ticks>(Ticks);

impl<const B_FREQ: Ticks> TickConverter<B_FREQ> {
    pub fn add_a_ticks(&mut self, a_ticks: Ticks) {
        self.0 += B_FREQ * a_ticks;
    }

    pub fn get_b_ticks(&self, a_freq: Ticks) -> Ticks {
        self.0 / a_freq
    }

    pub fn subtract_b_ticks(&mut self, b_ticks: Ticks, a_freq: Ticks) {
        self.0 -= a_freq * b_ticks;
    }
}
