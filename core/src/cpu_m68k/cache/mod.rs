//! M68k CPU - On-chip caches
//!
//! A cache model sits between address translation and the bus: it gets each
//! part of an operand access, and instruction fetches, with their
//! translation, and runs the bus cycles they need (the access itself, line
//! fills, write backs) on the bus transaction of the access. MOVE16 line
//! transfers go through it the same way.

mod m68020;
mod m68030;
mod m68040;

pub use m68020::Caches020;
pub use m68030::Caches030;
pub use m68040::Caches040;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, InspectableBus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::{AccessKind, CpuAccess};
use crate::cpu_m68k::bus_transaction::BusTransaction;
use crate::cpu_m68k::cpu::HistoryEntryInstruction;
use crate::cpu_m68k::pmmu::translate::Translation;
use crate::cpu_m68k::regs::RegisterCACR;
use crate::tickable::Ticks;
use crate::types::{Byte, Long};

/// Which caches CINV and CPUSH act on
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheSelect {
    Data,
    Instruction,
    Both,
}

/// What CINV and CPUSH act on
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheScope {
    /// The line holding an address
    Line,
    /// The page holding an address, of the given size in bytes
    Page(Address),
    All,
}

/// The on-chip caches of a CPU model
///
/// Bus errors, from any of the cycles a cache model runs, are returned as
/// they come: a cache model only changes its state once the bus cycles it
/// depends on have succeeded.
pub trait CpuCaches: Default {
    /// Runs the part of an operand access from `acc.laddr` on, translated
    /// to `tr`, through the caches and on `bus`. Returns the read data and
    /// the number of bytes done. Without caches in the way, the access runs
    /// on the bus as it is.
    #[inline(always)]
    fn access<TBus, const CPU_TYPE: CpuM68kType>(
        &mut self,
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        _cacr: RegisterCACR,
        acc: &CpuAccess,
        tr: &Translation,
    ) -> Result<(Long, u32)>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata)
    }

    /// Runs the part of a read access of `size` bytes left in the operand
    /// from logical address `laddr` on from the caches, before it is
    /// translated, if it hits in caches tagged with logical addresses: adds
    /// the time it takes to `ticks` and returns the read data and the number
    /// of bytes done. Returns None when the access has to be translated and
    /// run with access().
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn read_hit(
        &mut self,
        _cacr: RegisterCACR,
        _kind: AccessKind,
        _fc: u8,
        _laddr: Address,
        _size: u32,
        _ticks: &mut Ticks,
        _history: &mut HistoryEntryInstruction,
    ) -> Option<(Long, u32)> {
        None
    }

    /// MOVEC to CACR: runs the cache operations the value asks for (clear,
    /// clear entry) and returns the value to keep in CACR
    fn write_cacr(&mut self, val: Long, _caar: Address) -> Long {
        val
    }

    /// CINV and CPUSH (68040): invalidates, and for CPUSH first writes
    /// back, the lines of `scope` at physical address `paddr`
    fn invalidate<TBus, const CPU_TYPE: CpuM68kType>(
        &mut self,
        _bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        _caches: CacheSelect,
        _scope: CacheScope,
        _paddr: Address,
        _push: bool,
    ) -> Result<()>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        Ok(())
    }

    /// Runs the line transfer of `acc` (MOVE16), translated to `tr`,
    /// through the caches and on `bus`: reads the line, or writes `wline`.
    /// Returns the read line. Without caches in the way, the line runs on
    /// the bus as it is.
    #[inline(always)]
    fn line_access<TBus, const CPU_TYPE: CpuM68kType>(
        &mut self,
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        _cacr: RegisterCACR,
        acc: &CpuAccess,
        tr: &Translation,
        wline: Option<&[Long; 4]>,
    ) -> Result<[Long; 4]>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        bus.line_access(&acc.fault, tr.paddr, wline)
    }

    /// Makes the caches and memory agree before the host (debugger, mouse
    /// emulation) reads, or if `write` writes, `len` bytes of memory at
    /// physical address `paddr` behind the CPU's back: dirty data in the
    /// range is written back to memory, and before a write the cached copies
    /// are invalidated. Takes no CPU time.
    fn host_flush<TBus>(&mut self, _bus: &mut TBus, _paddr: Address, _len: Address, _write: bool)
    where
        TBus: InspectableBus<Address, Byte>,
    {
    }

    /// Hardware reset
    fn reset(&mut self) {}
}

/// No on-chip caches: every access goes to the bus
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NoCaches;

impl CpuCaches for NoCaches {}
