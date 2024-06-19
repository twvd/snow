use anyhow::Result;

/// Representation of X amount of ticks (T-cycles)
/// of the main system clock.
pub type Ticks = usize;

pub trait Tickable {
    fn tick(&mut self, ticks: Ticks) -> Result<Ticks>;
}
