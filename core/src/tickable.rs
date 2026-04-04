use anyhow::Result;

/// Representation of X amount of ticks (T-cycles)
/// of the main system clock.
pub type Ticks = u64;

pub trait Tickable<TContext = ()> {
    fn tick(&mut self, ticks: Ticks, ctx: TContext) -> Result<Ticks>;
}
