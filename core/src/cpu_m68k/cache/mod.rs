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

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::{AccessKind, CpuAccess};
use crate::cpu_m68k::bus_transaction::BusTransaction;
use crate::cpu_m68k::cpu::HistoryEntryInstruction;
use crate::cpu_m68k::pmmu::translate::Translation;
use crate::cpu_m68k::regs::RegisterCACR;
use crate::tickable::Ticks;
use crate::types::{Byte, Long};

/// CPU cycles of a 68020/030 prefetch that hits in the instruction cache or
/// the cache holding register. The manuals give no delay for a hit
/// (MC68020UM 8.1.1, MC68030UM 11.2.3): these are rather the execution time
/// of a one-word instruction in the cache case (MOVE Rn,Dn: 2, tail 0,
/// MC68030UM 11.6.6). Charging them per word gives the same timings in
/// practice: a run of `move.l d1,d5` takes 2.0 clocks per instruction on a
/// real IIci and LC III, with every other word from the holding register.
pub(in crate::cpu_m68k) const FETCH_HIT_TICKS: Ticks = 2;

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

/// An entry of a cache tagged with logical addresses, as a physical address
/// may be in it: see CpuCaches::inspect_aliases()
#[derive(Clone, Copy, Debug)]
pub struct CacheAlias {
    /// AccessKind::Data for the data cache, AccessKind::Fetch for the
    /// instruction cache
    pub kind: AccessKind,
    pub fc: u8,
    pub laddr: Address,
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

    /// Runs a read of `size` bytes (within a long word) at physical address
    /// `paddr` from caches tagged with physical addresses, if it hits: as
    /// access() would, with `fetch` for an instruction fetch. Returns None
    /// when the access has to run with access().
    #[inline(always)]
    fn phys_read_hit(
        &mut self,
        _cacr: RegisterCACR,
        _fetch: bool,
        _fc: u8,
        _paddr: Address,
        _size: u32,
        _history: &mut HistoryEntryInstruction,
    ) -> Option<(Long, u32)> {
        None
    }

    /// Runs a write of the low `size` bytes (within a long word) of `data`
    /// at physical address `paddr` of a copyback page in caches tagged with
    /// physical addresses, if it hits: as access() would. Returns false
    /// when the access has to run with access().
    #[inline(always)]
    fn phys_write_hit(
        &mut self,
        _cacr: RegisterCACR,
        _fc: u8,
        _paddr: Address,
        _size: u32,
        _data: Long,
    ) -> bool {
        false
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

    /// Returns the byte at physical address `paddr` if caches tagged with
    /// physical addresses would serve a data read of it, for the host. This
    /// changes nothing in the caches and takes no CPU time.
    fn inspect_phys_read(&self, _cacr: RegisterCACR, _paddr: Address) -> Option<Byte> {
        None
    }

    /// Returns whether the byte at physical address `paddr` is in data of a
    /// cache that a data read would hit and that memory doesn't have yet.
    fn inspect_phys_dirty(&self, _cacr: RegisterCACR, _paddr: Address) -> bool {
        false
    }

    /// Replaces the byte at physical address `paddr` in the caches tagged
    /// with physical addresses that hold it, for the host. This only changes
    /// the data: a line stays as clean or dirty as it is, enabled or not.
    fn inspect_phys_write(&mut self, _paddr: Address, _val: Byte) {}

    /// Returns the byte at logical address `laddr` if caches tagged with
    /// logical addresses would serve a read of it, as read_hit() would, for
    /// the host. This changes nothing in the caches and takes no CPU time.
    fn inspect_logical_read(
        &self,
        _cacr: RegisterCACR,
        _kind: AccessKind,
        _fc: u8,
        _laddr: Address,
    ) -> Option<Byte> {
        None
    }

    // A cache tagged with logical addresses can't be searched by physical address.
    // But pages are at least 256 bytes, so the low address bits that select an entry
    // are the same in both, and a physical address can only be in one entry per cache.
    // inspect_aliases() returns those entries, and the CPU, as the caches can't reach
    // the MMU, translates the address each holds to see if it really is that physical
    // address, before reading it or replacing a byte with inspect_alias_write().
    /// Returns the valid entries of caches tagged with logical addresses, if
    /// any, that may hold physical address `paddr`: its low address bits
    /// select the entry, as pages are at least 256 bytes, and only translating
    /// the address the entry holds can tell whether it is for `paddr`. The
    /// data cache comes first.
    fn inspect_aliases(&self, _paddr: Address) -> [Option<CacheAlias>; 2] {
        [None, None]
    }

    /// Replaces a byte in an entry from inspect_aliases(), for the host
    fn inspect_alias_write(&mut self, _alias: &CacheAlias, _val: Byte) {}

    /// Hardware reset
    fn reset(&mut self) {}
}

/// No on-chip caches: every access goes to the bus
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NoCaches;

impl CpuCaches for NoCaches {}
