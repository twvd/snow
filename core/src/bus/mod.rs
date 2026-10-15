pub mod access;
pub mod testbus;

use crate::bus::access::{BusAccess, BusResponse, CpuBus};
use crate::tickable::{Tickable, Ticks};

use anyhow::Result;
use num_traits::{PrimInt, WrappingAdd};

/// Main CPU address data type (actually 24-bit)
pub type Address = u32;

pub trait BusMember<T: PrimInt> {
    fn read(&mut self, addr: T) -> Option<u8>;
    fn write(&mut self, addr: T, val: u8) -> Option<()>;
}

pub trait Bus<TA: PrimInt + WrappingAdd, TD: PrimInt>: Tickable {
    /// Data bus of the CPU the bus is attached to
    type Cpu: CpuBus;

    fn get_mask(&self) -> TA;

    /// Runs one bus cycle
    fn access(&mut self, acc: BusAccess<Self::Cpu>) -> Result<BusResponse>;

    /// RESET line triggered by 68k RESET instruction (soft) or
    /// reset caused by CPU external reset (hard).
    ///
    /// Returns 'true' if the reset asserted the HALT line which
    /// in turn should hard-reset the CPU itself.
    fn reset(&mut self, hard: bool) -> Result<bool>;

    fn cpu_tick(&mut self, _ticks: Ticks) -> Result<()> {
        Ok(())
    }
}

/// Inspectable provides an interface to debugging/memory views.
///
/// This view is different from the bus interface exposed to the CPU,
/// because this view should provide a view that has no read side effects, to
/// avoid debugging views from tampering peripheral states.
///
/// This may also mean that this view on the bus is limited to e.g. ROM/RAM
/// areas and no peripherals.
pub trait InspectableBus<TA: PrimInt + WrappingAdd, TD: PrimInt> {
    fn inspect_read(&mut self, addr: TA) -> Option<TD>;
    fn inspect_write(&mut self, addr: TA, val: TD) -> Option<()>;
}

pub trait IrqSource {
    fn get_irq(&mut self) -> Option<u8>;
}
