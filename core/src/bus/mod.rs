pub mod access;
pub mod native_clock;
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

    /// Returns whether a device has asked for a reset of the whole machine
    fn take_reset_request(&mut self) -> bool {
        false
    }

    fn cpu_tick(&mut self, _ticks: Ticks) -> Result<()> {
        Ok(())
    }

    /// Whether the SCC wants the LocalTalk bridge polled now
    fn localtalk_poll_needed(&self) -> bool {
        false
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
    /// Reads from `addr` on into `out` as far as one run goes: see InspectRun.
    /// This sees what inspect_read() does, but faster for large ranges.
    fn inspect_read_bulk(&mut self, addr: TA, out: &mut [TD]) -> InspectRun;
    /// Changes when the map of the physical addresses changes, or when
    /// anything but the CPU changes memory
    fn inspect_generation(&self) -> u32;
}

/// What InspectableBus::inspect_read_bulk() found at an address
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectRun {
    /// This many bytes could be read, and are in the output
    Data(usize),
    /// This many bytes can't be read: nothing went to the output
    Hole(usize),
}

/// inspect_read_bulk() through inspect_read(), a byte at a time. What can be
/// read must be constant over blocks of `granule` bytes, a power of two, so
/// that a hole can be skipped as a whole.
pub fn inspect_read_bytes<TBus>(
    bus: &mut TBus,
    addr: Address,
    out: &mut [u8],
    granule: Address,
) -> InspectRun
where
    TBus: InspectableBus<Address, u8>,
{
    let max = out.len().min((granule - (addr & (granule - 1))) as usize);
    let mut n = 0;
    while n < max {
        let Some(byte) = bus.inspect_read(addr.wrapping_add(n as Address)) else {
            break;
        };
        out[n] = byte;
        n += 1;
    }
    if n > 0 {
        InspectRun::Data(n)
    } else {
        InspectRun::Hole(max)
    }
}

pub trait IrqSource {
    fn get_irq(&mut self) -> Option<u8>;
}
