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
/// Frequency A can be decided at runtime, while frequency B is set when
/// the converter is created.
///
/// Rational arithmetic is used to prevent errors from accumulating.
/// The struct holds the numerator `N` in `N / b_freq`, where `b_freq`
/// is the frequency of clock B.
#[derive(Serialize, Deserialize)]
pub struct TickConverter {
    n: Ticks,
    b_freq: Ticks,
}

impl TickConverter {
    pub fn new(b_freq: Ticks) -> Self {
        Self { n: 0, b_freq }
    }

    pub fn add_a_ticks(&mut self, a_ticks: Ticks) {
        self.n += self.b_freq * a_ticks;
    }

    pub fn get_b_ticks(&self, a_freq: Ticks) -> Ticks {
        self.n / a_freq
    }

    pub fn subtract_b_ticks(&mut self, b_ticks: Ticks, a_freq: Ticks) {
        self.n -= a_freq * b_ticks;
    }
}
