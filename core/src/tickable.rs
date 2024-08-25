use anyhow::Result;

/// Representation of X amount of ticks (T-cycles)
/// of the main system clock.
pub type Ticks = usize;

/// Main system clock speed in Hz
pub const TICKS_PER_SECOND: Ticks = 8_000_000;

pub trait Tickable {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks>;
}
