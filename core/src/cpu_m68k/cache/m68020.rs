//! M68k CPU - 68020 instruction cache
//!
//! 64 long word entries, direct mapped, tagged with address bits A31-A8
//! and FC2 (MC68020UM 7.1). It only holds instruction prefetches.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::{AccessKind, CpuAccess, low_bytes};
use crate::cpu_m68k::bus_transaction::BusTransaction;
use crate::cpu_m68k::cache::CpuCaches;
use crate::cpu_m68k::cpu::HistoryEntryInstruction;
use crate::cpu_m68k::pmmu::translate::Translation;
use crate::cpu_m68k::regs::RegisterCACR;
use crate::tickable::Ticks;
use crate::types::Long;

/// Number of entries
const ENTRIES: usize = 64;

/// Tag of an entry that holds nothing: a real tag has its bits 7-1 clear
const INVALID: u32 = u32::MAX;

/// CPU cycles of a prefetch that hits in the cache: in the cache case
/// timings of the 68020, the pipe takes two clocks per word
const HIT_TICKS: Ticks = 2;

/// The 68020 instruction cache
#[derive(Clone, Serialize, Deserialize)]
pub struct Caches020 {
    /// Address bits A31-A8 and FC2 in bit 0, or INVALID
    #[serde(with = "BigArray")]
    tags: [u32; ENTRIES],
    /// The cached long words
    #[serde(with = "BigArray")]
    data: [Long; ENTRIES],
}

impl Default for Caches020 {
    fn default() -> Self {
        Self {
            tags: [INVALID; ENTRIES],
            data: [0; ENTRIES],
        }
    }
}

impl Caches020 {
    /// Entry of logical address `addr`
    fn index(addr: Address) -> usize {
        (addr >> 2) as usize % ENTRIES
    }

    /// Tag of logical address `addr` in function code `fc`
    fn tag(addr: Address, fc: u8) -> u32 {
        (addr & 0xFFFF_FF00) | u32::from(fc >> 2)
    }
}

impl CpuCaches for Caches020 {
    /// A prefetch that hits is served before translation: the cache is
    /// tagged with logical addresses
    #[inline(always)]
    fn read_hit(
        &mut self,
        cacr: RegisterCACR,
        acc: &CpuAccess,
        ticks: &mut Ticks,
        history: &mut HistoryEntryInstruction,
    ) -> Option<(Long, u32)> {
        if acc.kind != AccessKind::Fetch || !cacr.e() {
            return None;
        }
        let index = Self::index(acc.laddr);
        if self.tags[index] != Self::tag(acc.laddr, acc.fault.fc) {
            return None;
        }
        let offset = acc.laddr % 4;
        let len = acc.size.min(4 - offset);
        history.icache_hit = true;
        *ticks += HIT_TICKS;
        Some((
            low_bytes(self.data[index] >> (8 * (4 - offset - len)), len),
            len,
        ))
    }

    #[inline(always)]
    fn access<TBus, const CPU_TYPE: CpuM68kType>(
        &mut self,
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        cacr: RegisterCACR,
        acc: &CpuAccess,
        tr: &Translation,
    ) -> Result<(Long, u32)>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        if acc.kind != AccessKind::Fetch || !cacr.e() {
            return bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata);
        }

        let index = Self::index(acc.laddr);
        let tag = Self::tag(acc.laddr, acc.fault.fc);
        let offset = acc.laddr % 4;
        let len = acc.size.min(4 - offset);

        if self.tags[index] == tag {
            bus.history.icache_hit = true;
            *bus.ticks += HIT_TICKS;
            return Ok((
                low_bytes(self.data[index] >> (8 * (4 - offset - len)), len),
                len,
            ));
        }

        bus.history.icache_miss = true;
        if cacr.f() {
            // Frozen: the entry is not replaced
            return bus.access(&acc.fault, tr.paddr, acc.size, None);
        }

        // A miss loads the whole long word
        let (long, _) = bus.access(&acc.fault, tr.paddr & !3, 4, None)?;
        self.tags[index] = tag;
        self.data[index] = long;
        Ok((low_bytes(long >> (8 * (4 - offset - len)), len), len))
    }

    fn write_cacr(&mut self, val: Long, caar: Address) -> Long {
        let val = RegisterCACR(val);
        if val.c() {
            self.tags.fill(INVALID);
        }
        if val.ce() {
            self.tags[Self::index(caar)] = INVALID;
        }
        val.with_c(false).with_ce(false).0
    }

    fn reset(&mut self) {
        self.tags.fill(INVALID);
    }
}
