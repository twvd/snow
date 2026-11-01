//! M68k CPU - 68030 instruction and data caches
//!
//! Two caches of 16 lines of 4 long words, direct mapped, tagged with
//! address bits A31-A8 and the function code, with a valid bit per long
//! word (MC68030UM 6.1). The instruction cache holds instruction
//! prefetches, the data cache the other accesses but those to CPU space.
//! The data cache is write-through: every write goes to memory.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::{AccessKind, CpuAccess, low_bytes};
use crate::cpu_m68k::bus_transaction::BusTransaction;
use crate::cpu_m68k::cache::{CacheAlias, CacheTag, CachedLong, CpuCaches, FETCH_HIT_TICKS};
use crate::cpu_m68k::cpu::HistoryEntryInstruction;
use crate::cpu_m68k::pmmu::translate::{CacheMode, Translation};
use crate::cpu_m68k::regs::RegisterCACR;
use crate::tickable::Ticks;
use crate::types::{Byte, Long};

/// Number of lines
const LINES: usize = 16;

/// Function code of CPU space accesses, never cached
const FC_CPU_SPACE: u8 = 7;

/// CPU cycles of a data access that hits in the data cache
const DATA_HIT_TICKS: Ticks = 1;

/// One of the two caches
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cache030 {
    /// Address bits A31-A8 and the function code in bits 2-0
    tags: [u32; LINES],
    /// Valid bit per long word
    valid: [u8; LINES],
    /// The cached long words
    data: [[Long; 4]; LINES],
}

impl Cache030 {
    fn line(addr: Address) -> usize {
        (addr >> 4) as usize % LINES
    }

    fn entry(addr: Address) -> usize {
        (addr >> 2) as usize % 4
    }

    /// Tag of logical address `addr` in function code `fc`
    fn tag(addr: Address, fc: u8) -> u32 {
        (addr & 0xFFFF_FF00) | u32::from(fc)
    }

    /// Returns whether the long word holding `addr` is in the cache
    fn hit(&self, tag: u32, addr: Address) -> bool {
        let line = Self::line(addr);
        self.tags[line] == tag && self.valid[line] & (1 << Self::entry(addr)) != 0
    }

    /// Loads the long word holding `addr`. A line with another tag is
    /// replaced, with its other entries invalid.
    fn fill(&mut self, tag: u32, addr: Address, long: Long) {
        let line = Self::line(addr);
        if self.tags[line] != tag {
            self.tags[line] = tag;
            self.valid[line] = 0;
        }
        self.valid[line] |= 1 << Self::entry(addr);
        self.data[line][Self::entry(addr)] = long;
    }

    /// Loads the long words of `line` selected by `entries` into the line
    /// holding `addr`. A line with another tag is replaced, with its other
    /// entries invalid.
    fn fill_line(&mut self, tag: u32, addr: Address, data: &[Long; 4], entries: u8) {
        let line = Self::line(addr);
        if self.tags[line] != tag {
            self.tags[line] = tag;
            self.valid[line] = 0;
        }
        self.valid[line] |= entries;
        for (i, &long) in data.iter().enumerate() {
            if entries & (1 << i) != 0 {
                self.data[line][i] = long;
            }
        }
    }

    /// The byte at `addr` in the long word entry that holds it
    fn byte(&self, addr: Address) -> Byte {
        self.data[Self::line(addr)][Self::entry(addr)].to_be_bytes()[addr as usize % 4]
    }

    /// The logical address, with its function code, of the long word entry
    /// that may hold physical address `paddr`, if that entry is valid
    fn alias(&self, paddr: Address) -> Option<(u8, Address)> {
        let line = Self::line(paddr);
        if self.valid[line] & (1 << Self::entry(paddr)) == 0 {
            return None;
        }
        let tag = self.tags[line];
        Some(((tag & 0b111) as u8, (tag & 0xFFFF_FF00) | (paddr & 0xFF)))
    }

    /// Replaces the byte at `addr` in the long word entry that holds it
    fn write_byte(&mut self, addr: Address, val: Byte) {
        let long = &mut self.data[Self::line(addr)][Self::entry(addr)];
        let mut bytes = long.to_be_bytes();
        bytes[addr as usize % 4] = val;
        *long = Long::from_be_bytes(bytes);
    }

    /// Invalidates the long word entry of `addr`
    fn invalidate_entry(&mut self, addr: Address) {
        self.valid[Self::line(addr)] &= !(1 << Self::entry(addr));
    }

    /// Invalidates the whole cache
    fn invalidate(&mut self) {
        self.valid = [0; LINES];
    }
}

/// The 68030 instruction and data caches
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Caches030 {
    icache: Cache030,
    dcache: Cache030,
}

impl CpuCaches for Caches030 {
    /// A read that hits is served before translation: the caches are
    /// tagged with logical addresses, and the 68030 only searches the
    /// translation tables for a read that misses (MC68030UM 9.2.1)
    #[inline(always)]
    fn read_hit(
        &mut self,
        cacr: RegisterCACR,
        kind: AccessKind,
        fc: u8,
        laddr: Address,
        size: u32,
        ticks: &mut Ticks,
        history: &mut HistoryEntryInstruction,
    ) -> Option<(Long, u32)> {
        let (cache, tag) = match kind {
            AccessKind::Fetch if cacr.e() => (&self.icache, Cache030::tag(laddr, fc & 0b100)),
            AccessKind::Data if cacr.ed() && fc != FC_CPU_SPACE => {
                (&self.dcache, Cache030::tag(laddr, fc))
            }
            _ => return None,
        };
        if !cache.hit(tag, laddr) {
            return None;
        }
        if kind == AccessKind::Fetch {
            history.icache_hit = true;
            *ticks += FETCH_HIT_TICKS;
        } else {
            *ticks += DATA_HIT_TICKS;
        }
        let offset = laddr % 4;
        let len = size.min(4 - offset);
        let long = cache.data[Cache030::line(laddr)][Cache030::entry(laddr)];
        Some((low_bytes(long >> (8 * (4 - offset - len)), len), len))
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
        let fc = acc.fault.fc;
        let (cache, tag, enabled, frozen) = match acc.kind {
            // The instruction cache only tags FC2
            AccessKind::Fetch => (
                &mut self.icache,
                Cache030::tag(acc.laddr, fc & 0b100),
                cacr.e(),
                cacr.f(),
            ),
            AccessKind::Data if fc != FC_CPU_SPACE => (
                &mut self.dcache,
                Cache030::tag(acc.laddr, fc),
                cacr.ed(),
                cacr.fd(),
            ),
            _ => return bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata),
        };
        // A cache inhibited access ignores the caches, even on writes
        if !enabled || tr.cache == CacheMode::Inhibited {
            return bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata);
        }

        // The part of the access in this long word entry
        let offset = acc.laddr % 4;
        let len = acc.size.min(4 - offset);
        let hit = cache.hit(tag, acc.laddr);

        if let Some(wdata) = acc.wdata {
            // Write-through: a hit updates the entry. On a miss with write
            // allocation, an aligned long word write loads the entry and
            // any other write invalidates it (MC68030UM 6.1.2.1).
            let bytes = low_bytes(wdata >> (8 * (acc.size - len)), len);
            if hit {
                let shift = 8 * (4 - offset - len);
                let mask = low_bytes(Long::MAX, len) << shift;
                let long = &mut cache.data[Cache030::line(acc.laddr)][Cache030::entry(acc.laddr)];
                *long = (*long & !mask) | (bytes << shift);
            } else if cacr.wa() && !frozen {
                if len == 4 {
                    cache.fill(tag, acc.laddr, bytes);
                } else {
                    cache.invalidate_entry(acc.laddr);
                }
            }
            return bus.access(&acc.fault, tr.paddr, acc.size, acc.wdata);
        }

        if hit {
            if acc.kind == AccessKind::Fetch {
                bus.history.icache_hit = true;
                *bus.ticks += FETCH_HIT_TICKS;
            } else {
                *bus.ticks += DATA_HIT_TICKS;
            }
            let long = cache.data[Cache030::line(acc.laddr)][Cache030::entry(acc.laddr)];
            return Ok((low_bytes(long >> (8 * (4 - offset - len)), len), len));
        }

        if acc.kind == AccessKind::Fetch {
            bus.history.icache_miss = true;
        }
        if frozen {
            return bus.access(&acc.fault, tr.paddr, acc.size, None);
        }

        // A miss loads the whole long word, in as many cycles as the port
        // needs (MC68030UM 6.1.3.1), or the whole line if burst filling is
        // enabled and the port takes the burst
        let burst_enabled = if acc.kind == AccessKind::Fetch {
            cacr.ibe()
        } else {
            cacr.dbe()
        };
        let long = if burst_enabled {
            let (line, entries) = bus.line_fill_68030(&acc.fault, tr.paddr & !3)?;
            cache.fill_line(tag, acc.laddr, &line, entries);
            line[Cache030::entry(acc.laddr)]
        } else {
            let (long, _) = bus.access(&acc.fault, tr.paddr & !3, 4, None)?;
            cache.fill(tag, acc.laddr, long);
            long
        };
        Ok((low_bytes(long >> (8 * (4 - offset - len)), len), len))
    }

    fn write_cacr(&mut self, val: Long, caar: Address) -> Long {
        let val = RegisterCACR(val);
        if val.c() {
            self.icache.invalidate();
        }
        if val.ce() {
            self.icache.invalidate_entry(caar);
        }
        if val.cd() {
            self.dcache.invalidate();
        }
        if val.ced() {
            self.dcache.invalidate_entry(caar);
        }
        val.with_c(false)
            .with_ce(false)
            .with_cd(false)
            .with_ced(false)
            .0
    }

    fn inspect_logical_read(
        &self,
        cacr: RegisterCACR,
        kind: AccessKind,
        fc: u8,
        laddr: Address,
    ) -> Option<Byte> {
        let (cache, tag) = match kind {
            AccessKind::Fetch if cacr.e() => (&self.icache, Cache030::tag(laddr, fc & 0b100)),
            AccessKind::Data if cacr.ed() && fc != FC_CPU_SPACE => {
                (&self.dcache, Cache030::tag(laddr, fc))
            }
            _ => return None,
        };
        cache.hit(tag, laddr).then(|| cache.byte(laddr))
    }

    fn inspect_aliases(&self, paddr: Address) -> [Option<CacheAlias>; 2] {
        let alias = |kind, (fc, laddr)| CacheAlias { kind, fc, laddr };
        [
            self.dcache.alias(paddr).map(|a| alias(AccessKind::Data, a)),
            // The instruction cache only tags FC2
            self.icache
                .alias(paddr)
                .map(|(fc, laddr)| alias(AccessKind::Fetch, (fc | 0b010, laddr))),
        ]
    }

    fn inspect_alias_write(&mut self, alias: &CacheAlias, val: Byte) {
        let cache = if alias.kind == AccessKind::Fetch {
            &mut self.icache
        } else {
            &mut self.dcache
        };
        cache.write_byte(alias.laddr, val);
    }

    fn inspect_cached(&self, cacr: RegisterCACR, kind: AccessKind) -> Vec<CachedLong> {
        let (cache, fc_or) = match kind {
            AccessKind::Data if cacr.ed() => (&self.dcache, 0),
            // The instruction cache only tags FC2
            AccessKind::Fetch if cacr.e() => (&self.icache, 0b010),
            _ => return Vec::new(),
        };
        let mut longs = Vec::new();
        for line in 0..LINES {
            let tag = cache.tags[line];
            for entry in 0..4 {
                if cache.valid[line] & (1 << entry) != 0 {
                    longs.push(CachedLong {
                        tag: CacheTag::Logical {
                            fc: (tag & 0b111) as u8 | fc_or,
                            laddr: (tag & 0xFFFF_FF00) | ((line as Address) << 4) | (entry << 2),
                        },
                        long: cache.data[line][entry as usize],
                    });
                }
            }
        }
        longs
    }

    fn reset(&mut self) {
        self.icache.invalidate();
        self.dcache.invalidate();
    }
}
