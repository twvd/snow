use anyhow::Result;

use crate::emulator::EmuContext;

/// Representation of X amount of ticks (T-cycles)
/// of the main system clock.
pub type Ticks = u64;

pub trait Tickable {
    fn tick(&mut self, ticks: Ticks, ctx: &dyn EmuContext) -> Result<Ticks>;
}
