use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Representation of X amount of ticks (T-cycles)
/// of the main system clock.
pub type Ticks = u64;

pub trait Tickable<TContext = ()> {
    fn tick(&mut self, ticks: Ticks, ctx: TContext) -> Result<Ticks>;
}

/// Converts ticks from one clock frequency (IN) to another (OUT).
///
/// Frequency IN can be decided at runtime, while frequency OUT must be
/// specified as a const generic.
///
/// Rational arithmetic is used to prevent errors.
/// The struct holds the numerator `N` in `N / OUT_FREQ`, where `OUT_FREQ`
/// is the frequency of clock OUT.
#[derive(Default, Serialize, Deserialize)]
pub struct TickConverter<const OUT_FREQ: Ticks>(Ticks);

impl<const OUT_FREQ: Ticks> TickConverter<OUT_FREQ> {
    pub fn add_in_ticks(&mut self, in_ticks: Ticks) {
        self.0 += OUT_FREQ * in_ticks;
    }

    pub fn get_out_ticks(&self, in_freq: Ticks) -> Ticks {
        self.0 / in_freq
    }

    pub fn subtract_out_ticks(&mut self, out_ticks: Ticks, in_freq: Ticks) {
        self.0 -= in_freq * out_ticks;
    }
}
