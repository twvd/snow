//! M68k CPU - 68040 instruction and data caches
//!
//! Two four-way set-associative caches of 64 sets of 16-byte lines,
//! indexed and tagged with the physical address (MC68040UM 4.1). The data
//! cache handles write-through and copyback pages: a write to a copyback
//! page that hits only updates the cache, the dirty line is written back
//! (pushed) when it is replaced or by CPUSH.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::bus::{Address, Bus, InspectableBus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::{AccessKind, CpuAccess, low_bytes};
use crate::cpu_m68k::bus_transaction::{BusTransaction, FaultInfo};
use crate::cpu_m68k::cache::{CacheScope, CacheSelect, CpuCaches};
use crate::cpu_m68k::cpu::HistoryEntryInstruction;
use crate::cpu_m68k::pmmu::translate::{CacheMode, Translation};
use crate::cpu_m68k::regs::RegisterCACR;
use crate::types::{Byte, Long};

/// Number of sets
const SETS: usize = 64;
/// Lines per set
const WAYS: usize = 4;

/// Tag of a line that holds nothing: a real tag has its top 10 bits clear
const INVALID: u32 = u32::MAX;

/// Function code of CPU space accesses, never cached
const FC_CPU_SPACE: u8 = 7;

/// One of the two caches
#[derive(Clone, Serialize, Deserialize)]
struct Cache040 {
    /// Physical address bits A31-A10 of each line of each set, or INVALID
    #[serde(with = "BigArray")]
    tags: [[u32; WAYS]; SETS],
    /// Dirty bit per long word (data cache only)
    #[serde(with = "BigArray")]
    dirty: [[u8; WAYS]; SETS],
    /// The cached lines
    #[serde(with = "BigArray")]
    data: [[[Long; 4]; WAYS]; SETS],
    /// Replacement counter (MC68040UM 4.1)
    counter: usize,
    /// Last fetched half-line, to move the replacement counter once per
    /// fetched half-line
    #[serde(default)]
    last_fetch: Address,
}

impl Default for Cache040 {
    fn default() -> Self {
        Self {
            tags: [[INVALID; WAYS]; SETS],
            dirty: [[0; WAYS]; SETS],
            data: [[[0; 4]; WAYS]; SETS],
            counter: 0,
            last_fetch: u32::MAX,
        }
    }
}

impl Cache040 {
    fn set(paddr: Address) -> usize {
        (paddr >> 4) as usize % SETS
    }

    fn tag(paddr: Address) -> u32 {
        paddr >> 10
    }

    fn entry(paddr: Address) -> usize {
        (paddr >> 2) as usize % 4
    }

    /// Address of the line of `set` holding `tag`
    fn line_addr(set: usize, tag: u32) -> Address {
        (tag << 10) | ((set as Address) << 4)
    }

    /// The line holding `paddr`, if any
    #[inline(always)]
    fn find(&self, paddr: Address) -> Option<usize> {
        let tag = Self::tag(paddr);
        self.tags[Self::set(paddr)].iter().position(|&t| t == tag)
    }

    /// Picks the line a new line of the set of `paddr` replaces: the first
    /// invalid one, else the one the replacement counter points to
    fn victim(&self, paddr: Address) -> usize {
        let set = &self.tags[Self::set(paddr)];
        match set.iter().position(|&t| t == INVALID) {
            Some(way) => way,
            None => self.counter,
        }
    }

    /// Writes `len` bytes into a cached long word, `shift` bits from its
    /// right end.
    fn update(&mut self, set: usize, way: usize, entry: usize, shift: u32, len: u32, bytes: Long) {
        let mask = low_bytes(Long::MAX, len) << shift;
        let long = &mut self.data[set][way][entry];
        *long = (*long & !mask) | (bytes << shift);
    }

    /// Accounts for an access, for the replacement counter. An operand
    /// access is at most one long word, which a read touches as one
    /// half-line and a copyback write as one line (MC68040UM 4.1).
    fn count(&mut self) {
        self.counter = (self.counter + 1) % WAYS;
    }

    /// Accounts for an instruction fetch, for the replacement counter:
    /// the prefetch fetches and keeps a half-line at a time, so the
    /// counter moves once per half-line (MC68040UM 4.1).
    fn count_fetch(&mut self, paddr: Address) {
        if (paddr >> 3) != (self.last_fetch >> 3) {
            self.counter = (self.counter + 1) % WAYS;
            self.last_fetch = paddr;
        }
    }
}

/// The 68040 instruction and data caches
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Caches040 {
    icache: Cache040,
    dcache: Cache040,
}

impl Caches040 {
    /// Writes back the dirty long words of a data cache line: a single long
    /// word on its own, more as a line transfer (MC68040UM 4.6.2). A push
    /// bus error reports the physical address, with TM=0 (MC68040UM 8.4.6).
    fn push<TBus, const CPU_TYPE: CpuM68kType>(
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        cache: &mut Cache040,
        set: usize,
        way: usize,
    ) -> Result<()>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        let dirty = cache.dirty[set][way];
        if dirty == 0 {
            return Ok(());
        }
        let addr = Cache040::line_addr(set, cache.tags[set][way]);
        if dirty.count_ones() == 1 {
            let entry = dirty.trailing_zeros() as usize;
            let addr = addr.wrapping_add(4 * entry as Address);
            let fault = FaultInfo {
                fc: 0,
                o_addr: addr,
                size: 4,
                write: true,
            };
            bus.access(&fault, addr, 4, Some(cache.data[set][way][entry]))?;
        } else {
            let fault = FaultInfo {
                fc: 0,
                o_addr: addr,
                size: 16,
                write: true,
            };
            bus.line_access(&fault, addr, Some(&cache.data[set][way]))?;
        }
        cache.dirty[set][way] = 0;
        Ok(())
    }

    /// Loads the line holding `paddr`, as a line transfer, into the line
    /// the replacement picks, after writing back that line if it is dirty.
    /// Returns the line loaded and where it went.
    fn fill<TBus, const CPU_TYPE: CpuM68kType>(
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        fault: &FaultInfo,
        cache: &mut Cache040,
        paddr: Address,
    ) -> Result<([Long; 4], usize)>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        let set = Cache040::set(paddr);
        let way = cache.victim(paddr);
        // The 68040 reads the new line before it writes back the old one
        let line = bus.line_access(fault, paddr & !0xF, None)?;
        Self::push(bus, cache, set, way)?;
        cache.tags[set][way] = Cache040::tag(paddr);
        cache.data[set][way] = line;
        Ok((line, way))
    }

    /// Writes back the dirty long words of a data cache line straight to
    /// memory, for host_flush(). Returns false if memory can't take them.
    fn host_push<TBus>(bus: &mut TBus, cache: &mut Cache040, set: usize, way: usize) -> bool
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let addr = Cache040::line_addr(set, cache.tags[set][way]);
        for entry in 0..4 {
            if cache.dirty[set][way] & (1 << entry) == 0 {
                continue;
            }
            let long_addr = addr.wrapping_add(4 * entry as Address);
            for (i, byte) in cache.data[set][way][entry]
                .to_be_bytes()
                .into_iter()
                .enumerate()
            {
                if bus
                    .inspect_write(long_addr.wrapping_add(i as Address), byte)
                    .is_none()
                {
                    return false;
                }
            }
            cache.dirty[set][way] &= !(1 << entry);
        }
        true
    }

    /// A cache inhibited access, or MOVE16 writing over it, takes a line out
    /// of the cache: written back first if dirty and `push`
    fn evict<TBus, const CPU_TYPE: CpuM68kType>(
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        cache: &mut Cache040,
        paddr: Address,
        push: bool,
    ) -> Result<()>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        if let Some(way) = cache.find(paddr) {
            let set = Cache040::set(paddr);
            if push {
                Self::push(bus, cache, set, way)?;
            }
            cache.tags[set][way] = INVALID;
            cache.dirty[set][way] = 0;
        }
        Ok(())
    }
}

impl CpuCaches for Caches040 {
    #[inline(always)]
    fn phys_read_hit(
        &mut self,
        cacr: RegisterCACR,
        fetch: bool,
        fc: u8,
        paddr: Address,
        size: u32,
        history: &mut HistoryEntryInstruction,
    ) -> Option<(Long, u32)> {
        let cache = if fetch {
            if !cacr.ie() {
                return None;
            }
            &mut self.icache
        } else {
            if fc == FC_CPU_SPACE || !cacr.de() {
                return None;
            }
            &mut self.dcache
        };
        let way = cache.find(paddr)?;
        if fetch {
            cache.count_fetch(paddr);
            history.icache_hit = true;
        } else {
            cache.count();
        }
        let offset = paddr % 4;
        let len = size.min(4 - offset);
        let shift = 8 * (4 - offset - len);
        let long = cache.data[Cache040::set(paddr)][way][Cache040::entry(paddr)];
        Some((low_bytes(long >> shift, len), len))
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
        let fetch = acc.kind == AccessKind::Fetch;
        let (cache, enabled) = match acc.kind {
            AccessKind::Fetch => (&mut self.icache, cacr.ie()),
            AccessKind::Data if acc.fault.fc == FC_CPU_SPACE => {
                return bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata);
            }
            AccessKind::Data | AccessKind::Physical => (&mut self.dcache, cacr.de()),
        };
        if !enabled {
            return bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata);
        }
        if fetch {
            cache.count_fetch(tr.paddr);
        } else {
            cache.count();
        }

        // A cache inhibited access writes back and invalidates a line it
        // hits (MC68040UM 4.3.2)
        let paddr = tr.paddr;
        if tr.cache == CacheMode::Inhibited {
            Self::evict(bus, cache, paddr, true)?;
            return bus.access(&acc.fault, paddr, acc.size, acc.wdata);
        }

        // The part of the access in this long word, and where it is
        let offset = paddr % 4;
        let len = acc.size.min(4 - offset);
        let shift = 8 * (4 - offset - len);
        let set = Cache040::set(paddr);
        let entry = Cache040::entry(paddr);
        // Translation table searches that miss don't allocate (4.3.3)
        let allocate = acc.kind != AccessKind::Physical;
        let hit = cache.find(paddr);

        let Some(wdata) = acc.wdata else {
            // Read
            if let Some(way) = hit {
                // A hit takes no time of its own: the instruction times of the
                // 68040 are those of accesses that hit (MC68040UM 10.1)
                if fetch {
                    bus.history.icache_hit = true;
                }
                return Ok((low_bytes(cache.data[set][way][entry] >> shift, len), len));
            }
            if fetch {
                bus.history.icache_miss = true;
            }
            if !allocate {
                return bus.access(&acc.fault, paddr, acc.size, None);
            }
            let (line, _) = Self::fill(bus, &acc.fault, cache, paddr)?;
            return Ok((low_bytes(line[entry] >> shift, len), len));
        };

        // Write: the bytes of this long word
        let bytes = low_bytes(wdata >> (8 * (acc.size - len)), len);
        let way = match hit {
            // Write-through: memory is written, a line hit is updated, a
            // miss doesn't allocate (4.3.1.1)
            Some(way) if tr.cache == CacheMode::WriteThrough => {
                let res = bus.access(&acc.fault, paddr, acc.size, acc.wdata)?;
                cache.update(set, way, entry, shift, len, bytes);
                return Ok(res);
            }
            None if tr.cache == CacheMode::WriteThrough || !allocate => {
                return bus.access(&acc.fault, paddr, acc.size, acc.wdata);
            }
            // Copyback: a hit only updates the line and marks it dirty, a
            // miss loads the line first (4.3.1.2)
            Some(way) => way,
            None => Self::fill(bus, &acc.fault, cache, paddr)?.1,
        };
        cache.update(set, way, entry, shift, len, bytes);
        cache.dirty[set][way] |= 1 << entry;
        Ok((0, len))
    }

    fn line_access<TBus, const CPU_TYPE: CpuM68kType>(
        &mut self,
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        cacr: RegisterCACR,
        acc: &CpuAccess,
        tr: &Translation,
        wline: Option<&[Long; 4]>,
    ) -> Result<[Long; 4]>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        let paddr = tr.paddr;
        let cache = &mut self.dcache;
        if !cacr.de() {
            return bus.line_access(&acc.fault, paddr, wline);
        }
        cache.count();
        if tr.cache == CacheMode::Inhibited {
            Self::evict(bus, cache, paddr, true)?;
            return bus.line_access(&acc.fault, paddr, wline);
        }
        // MOVE16 doesn't allocate: a read that hits is served by the cache,
        // a write that hits invalidates the line, which it overwrites in
        // memory as a whole (MC68040UM 4.3.3)
        if wline.is_some() {
            Self::evict(bus, cache, paddr, false)?;
            return bus.line_access(&acc.fault, paddr, wline);
        }
        match cache.find(paddr) {
            Some(way) => Ok(cache.data[Cache040::set(paddr)][way]),
            None => bus.line_access(&acc.fault, paddr, None),
        }
    }

    fn invalidate<TBus, const CPU_TYPE: CpuM68kType>(
        &mut self,
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        caches: CacheSelect,
        scope: CacheScope,
        paddr: Address,
        push: bool,
    ) -> Result<()>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        // The lines in scope
        let mask = match scope {
            CacheScope::Line => !0xF,
            CacheScope::Page(size) => !(size - 1),
            CacheScope::All => 0,
        };
        if caches != CacheSelect::Data {
            Self::invalidate_cache(bus, &mut self.icache, paddr, mask, false)?;
        }
        if caches != CacheSelect::Instruction {
            Self::invalidate_cache(bus, &mut self.dcache, paddr, mask, push)?;
        }
        Ok(())
    }

    fn host_flush<TBus>(&mut self, bus: &mut TBus, paddr: Address, len: Address, write: bool)
    where
        TBus: InspectableBus<Address, Byte>,
    {
        if len == 0 {
            return;
        }
        let first = paddr & !0xF;
        let last = paddr.wrapping_add(len - 1) & !0xF;
        for line in (first..=last).step_by(16) {
            if let Some(way) = self.dcache.find(line) {
                let set = Cache040::set(line);
                // With nowhere to write the data back to, the line stays
                if Self::host_push(bus, &mut self.dcache, set, way) && write {
                    self.dcache.tags[set][way] = INVALID;
                }
            }
            if write && let Some(way) = self.icache.find(line) {
                self.icache.tags[Cache040::set(line)][way] = INVALID;
            }
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

impl Caches040 {
    /// CINV and CPUSH on one cache: invalidates the lines whose address
    /// matches `paddr` under `mask`, written back first if `push`
    fn invalidate_cache<TBus, const CPU_TYPE: CpuM68kType>(
        bus: &mut BusTransaction<'_, TBus, CPU_TYPE>,
        cache: &mut Cache040,
        paddr: Address,
        mask: Address,
        push: bool,
    ) -> Result<()>
    where
        TBus: Bus<Address, u8> + IrqSource,
    {
        for set in 0..SETS {
            for way in 0..WAYS {
                let tag = cache.tags[set][way];
                if tag == INVALID || Cache040::line_addr(set, tag) & mask != paddr & mask {
                    continue;
                }
                if push {
                    Self::push(bus, cache, set, way)?;
                }
                cache.tags[set][way] = INVALID;
                cache.dirty[set][way] = 0;
            }
        }
        Ok(())
    }
}
