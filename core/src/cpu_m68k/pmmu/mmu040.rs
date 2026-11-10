//! M68040 MMU - address translation
//!
//! Only supports 3 fixed levels of tables and 4K/8K page sizes
//! This module only contains the (much simpler) 68040 MMU logic,
//! which hooks into the PMMU logic.
//! Sorry if this is messy/hard to follow

use crate::bus::{Address, Bus, InspectableBus, IrqSource};
use crate::cpu_m68k::cache::CpuCaches;
use crate::cpu_m68k::cpu::{CpuError, CpuM68k, PagefaultCause};
use crate::cpu_m68k::pmmu::atc040::{ATC040_DATA, ATC040_INSTRUCTION, Atc040, Atc040Entry};
use crate::cpu_m68k::pmmu::translate::{CacheMode, Translation};
use crate::cpu_m68k::{CpuM68kType, M68040};
use crate::types::{Byte, Long};

use anyhow::{Result, anyhow, bail};

const DESC_TYPE: Long = 0b11;
const DESC_TYPE_INVALID: Long = 0;
const DESC_TYPE_INDIRECT: Long = 2;
const DESC_UDT_RESIDENT: Long = 1 << 1;
const DESC_PDT_RESIDENT: Long = 1 << 0;
const DESC_WP: Long = 1 << 2;
const DESC_U: Long = 1 << 3;
const DESC_M: Long = 1 << 4;
const DESC_S: Long = 1 << 7;
const DESC_G: Long = 1 << 10;

const TC_ENABLE: Long = 1 << 15;
const TC_PAGESIZE_8K: Long = 1 << 14;

const MMUSR_S: Long = 1 << 7;
const MMUSR_W: Long = 1 << 2;
const MMUSR_T: Long = 1 << 1;
const MMUSR_R: Long = 1 << 0;

const TT_ENABLE: Long = 1 << 15;
const TT_WP: Long = 1 << 2;

/// Result of a table walk
struct Walk040 {
    /// Physical address
    paddr: Address,
    /// Write protected by any descriptor of the walk
    wp: bool,
    /// Address of the page descriptor
    desc_addr: Address,
    /// Page descriptor, as read before the walk set its U bit
    desc: Long,
}

/// Cache mode (CM) field of page descriptors and TT registers
const CM_MASK: Long = 0b11 << 5;
const CM_WRITETHROUGH: Long = 0b00 << 5;
const CM_COPYBACK: Long = 0b01 << 5;

/// Cache mode of a page descriptor or TT register
const fn cache_mode(desc: Long) -> CacheMode {
    match desc & CM_MASK {
        CM_WRITETHROUGH => CacheMode::WriteThrough,
        CM_COPYBACK => CacheMode::CopyBack,
        _ => CacheMode::Inhibited,
    }
}

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, TCaches>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, TCaches>
where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    /// Amount of address bits within a page (12 = 4KB, 13 = 8KB)
    #[inline(always)]
    pub(in crate::cpu_m68k) fn mmu040_page_shift(&self) -> u32 {
        if self.regs.mmu040.tc & TC_PAGESIZE_8K != 0 {
            13
        } else {
            12
        }
    }

    #[inline(always)]
    pub(in crate::cpu_m68k) fn mmu040_enabled(&self) -> bool {
        self.regs.mmu040.tc & TC_ENABLE != 0
    }

    /// Returns the write protection and cache mode of the first enabled TT
    /// register that transparently maps this access, if any. TT regions
    /// bypass the page tables and the ATC entirely.
    ///
    /// Returns None if transparent translation is disabled.
    fn mmu040_tt_match(&self, fc: u8, vaddr: Address) -> Option<(bool, CacheMode)> {
        // FC: 1/5 = data, 2/6 = program
        let tts = if fc & 1 != 0 {
            &self.regs.mmu040.dtt
        } else {
            &self.regs.mmu040.itt
        };

        for &tt in tts {
            if tt & TT_ENABLE == 0 {
                continue;
            }

            // Supervisor/user field: 0 = user only, 1 = supervisor only,
            // 2 and 3 = both.
            let supervisor = fc & (1 << 2) != 0;
            match (tt >> 13) & 0b11 {
                0 if supervisor => continue,
                1 if !supervisor => continue,
                _ => (),
            }

            let mask = (!(tt >> 16) & 0xFF) << 24;
            if (vaddr & mask) != (tt & mask) {
                continue;
            }

            return Some((tt & TT_WP != 0, cache_mode(tt)));
        }

        None
    }

    /// Reads a descriptor and sets its U bit if any of the `resident` bits
    /// of its type are set
    fn mmu040_read_descriptor(&mut self, addr: Address, resident: Long) -> Result<Long> {
        let desc = self.read_ticks_physical::<Long>(addr)?;
        if desc & resident != 0 && desc & DESC_U == 0 {
            self.write_ticks_physical::<Long>(addr, desc | DESC_U)?;
        }
        Ok(desc)
    }

    /// Walks the page tables for one address, reading descriptors with `read`,
    /// which gets the address of a descriptor and the resident bits of its type.
    /// Returns None if the address has no translation.
    #[inline(always)]
    fn mmu040_walk(
        &mut self,
        fc: u8,
        vaddr: Address,
        mut read: impl FnMut(&mut Self, Address, Long) -> Result<Long>,
    ) -> Result<Option<Walk040>> {
        debug_assert_eq!(CPU_TYPE, M68040);

        let root_ptr = if fc & (1 << 2) != 0 {
            self.regs.mmu040.srp
        } else {
            self.regs.mmu040.urp
        };

        // Level 1: root table (128 entries)
        let root_addr = root_ptr.wrapping_add(((vaddr >> 25) & 0x7F) * 4);
        let root_desc = read(self, root_addr, DESC_UDT_RESIDENT)?;
        if root_desc & DESC_UDT_RESIDENT == 0 {
            return Ok(None);
        }

        // Level 2: pointer table (128 entries, 512 byte aligned)
        let ptr_addr = (root_desc & !0x1FF).wrapping_add(((vaddr >> 18) & 0x7F) * 4);
        let ptr_desc = read(self, ptr_addr, DESC_UDT_RESIDENT)?;
        if ptr_desc & DESC_UDT_RESIDENT == 0 {
            return Ok(None);
        }

        // Level 3: page table (64 entries for 4KB pages, 32 for 8KB)
        let (page_shift, table_mask) = if self.regs.mmu040.tc & TC_PAGESIZE_8K != 0 {
            (13, !0x7F)
        } else {
            (12, !0xFF)
        };
        let page_idx = (vaddr >> page_shift) & if page_shift == 13 { 0x1F } else { 0x3F };
        let mut leaf_addr = (ptr_desc & table_mask).wrapping_add(page_idx * 4);
        let mut leaf_desc = read(self, leaf_addr, DESC_PDT_RESIDENT)?;

        // A leaf descriptor may point at another descriptor. Exactly one level
        // of indirection is allowed; anything deeper is invalid.
        if leaf_desc & DESC_TYPE == DESC_TYPE_INDIRECT {
            leaf_addr = leaf_desc & !DESC_TYPE;
            leaf_desc = read(self, leaf_addr, DESC_PDT_RESIDENT)?;
            if leaf_desc & DESC_TYPE == DESC_TYPE_INDIRECT {
                return Ok(None);
            }
        }
        if leaf_desc & DESC_TYPE == DESC_TYPE_INVALID {
            return Ok(None);
        }

        // Write protection is inherited from every level of the walk
        let wp = (root_desc | ptr_desc | leaf_desc) & DESC_WP != 0;
        let page_mask = (1 << page_shift) - 1;

        Ok(Some(Walk040 {
            paddr: (leaf_desc & !page_mask) | (vaddr & page_mask),
            wp,
            desc_addr: leaf_addr,
            desc: leaf_desc,
        }))
    }

    /// Walks the page tables for one address
    fn mmu040_translate_lookup(&mut self, fc: u8, vaddr: Address) -> Result<Walk040> {
        let Some(walk) = self.mmu040_walk(fc, vaddr, Self::mmu040_read_descriptor)? else {
            bail!(CpuError::Pagefault(PagefaultCause::Invalid));
        };
        Ok(walk)
    }

    /// The ATC an access goes through: function codes 1 and 5 are data, 2
    /// and 6 program, as for the TT registers
    fn atc040_index(fc: u8) -> usize {
        if fc & 1 != 0 {
            ATC040_DATA
        } else {
            ATC040_INSTRUCTION
        }
    }

    /// Searches the tables for the page holding `vaddr` and stores the result
    /// in an ATC, as the 68040 does on an ATC miss, on a write to a page not
    /// marked modified, and for PTEST. An invalid descriptor makes an entry
    /// that isn't resident, which faults until it is flushed or replaced
    /// (MC68040UM 3.5). Returns the entry and the address of the page
    /// descriptor, if there is one.
    fn mmu040_search(
        &mut self,
        atc: usize,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<(Atc040Entry, Option<Address>)> {
        let supervisor = fc & (1 << 2) != 0;
        let page_shift = self.mmu040_page_shift();
        let page_mask = (1 << page_shift) - 1;
        let tag = Atc040::tag(vaddr, page_mask, supervisor);
        let (entry, desc_addr) = match self.mmu040_translate_lookup(fc, vaddr) {
            Ok(walk) => {
                let s = walk.desc & DESC_S != 0;
                // M is set for a write, unless it faults (MC68040UM 3.2.5)
                let mut modified = walk.desc & DESC_M != 0;
                if writing && !modified && !walk.wp && (supervisor || !s) {
                    let desc = self.read_ticks_physical::<Long>(walk.desc_addr)?;
                    self.write_ticks_physical::<Long>(walk.desc_addr, desc | DESC_M)?;
                    modified = true;
                }
                let entry = Atc040Entry {
                    tag,
                    paddr: walk.paddr & !page_mask,
                    resident: true,
                    wp: walk.wp,
                    s,
                    global: walk.desc & DESC_G != 0,
                    modified,
                    cache: cache_mode(walk.desc),
                };
                (entry, Some(walk.desc_addr))
            }
            Err(e) => match e.downcast_ref() {
                Some(CpuError::Pagefault(_)) => {
                    let entry = Atc040Entry {
                        tag,
                        ..Default::default()
                    };
                    (entry, None)
                }
                _ => return Err(e),
            },
        };
        if let Some(old) = self.atc040[atc].insert(entry, page_shift) {
            self.last_translation_forget(Atc040::tag_page(old), Atc040::tag_supervisor(old));
        }
        Ok((entry, desc_addr))
    }

    pub(in crate::cpu_m68k) fn mmu040_ptest(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<()> {
        // A transparent translation hit reports the logical address as the
        // physical one and no attributes beyond T/R and write protection.
        if let Some((wp, _)) = self.mmu040_tt_match(fc, vaddr) {
            self.regs.mmu040.mmusr =
                (vaddr & !0xFFF) | MMUSR_T | MMUSR_R | if wp { MMUSR_W } else { 0 };
            return Ok(());
        }

        if !self.mmu040_enabled() {
            self.regs.mmu040.mmusr = (vaddr & !0xFFF) | MMUSR_R;
            return Ok(());
        }

        // PTEST flushes the entry for the address from the ATC that DFC
        // selects, searches the tables and loads the result into that ATC
        // (MC68040UM 3.7.3). PTESTW sets M as a write would (M68000PRM 6-70).
        let supervisor = fc & (1 << 2) != 0;
        let page_shift = self.mmu040_page_shift();
        let tag = Atc040::tag(vaddr, (1 << page_shift) - 1, supervisor);
        let atc = Self::atc040_index(fc);
        self.atc040[atc].flush_tag(tag, page_shift, false);
        self.last_translation_forget(Atc040::tag_page(tag), supervisor);
        let (entry, desc_addr) = self.mmu040_search(atc, fc, vaddr, writing)?;
        let Some(desc_addr) = desc_addr else {
            self.regs.mmu040.mmusr = 0;
            return Ok(());
        };
        let desc = self.read_ticks_physical::<Long>(desc_addr)?;

        // Bits 10-4 of the leaf descriptor (G, U1/U0, S, CM, M) sit in
        // the same places in MMUSR. Write protection accumulates over
        // the whole walk, so it comes from the entry instead.
        let mut mmusr = entry.paddr | (vaddr & ((1 << page_shift) - 1) & !0xFFF);
        mmusr |= (desc & 0x0000_07F0) | MMUSR_R;
        if entry.wp {
            mmusr |= MMUSR_W;
        }
        if entry.s {
            mmusr |= MMUSR_S;
        }
        self.regs.mmu040.mmusr = mmusr;
        Ok(())
    }

    /// Translates a logical to a physical address
    pub(in crate::cpu_m68k) fn mmu040_translate(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<Translation> {
        debug_assert_eq!(CPU_TYPE, M68040);

        // Transparent translation is in effect even when paging is disabled
        if let Some((wp, cache)) = self.mmu040_tt_match(fc, vaddr) {
            if writing && wp {
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
            return Ok(Translation {
                paddr: vaddr,
                cache,
            });
        }

        // Without translation accesses are cachable, write-through
        // (MC68040UM 4.3)
        if !self.mmu040_enabled() {
            return Ok(Translation {
                paddr: vaddr,
                cache: CacheMode::WriteThrough,
            });
        }

        let supervisor = fc & (1 << 2) != 0;
        let page_shift = self.mmu040_page_shift();
        let page_mask = (1 << page_shift) - 1;
        let atc = Self::atc040_index(fc);
        let tag = Atc040::tag(vaddr, page_mask, supervisor);
        let entry = match self.atc040[atc].lookup(tag, page_shift) {
            // A write that may proceed to a page not marked modified searches
            // the tables again, to set M (MC68040UM 3.3, M bit)
            Some(e) if writing && !e.modified && e.resident && !e.wp && (supervisor || !e.s) => {
                self.mmu040_search(atc, fc, vaddr, writing)?.0
            }
            Some(e) => e,
            None => self.mmu040_search(atc, fc, vaddr, writing)?.0,
        };
        if !entry.resident || (!supervisor && entry.s) || (writing && entry.wp) {
            self.pmmu_record_pagefault(vaddr, writing);
            return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
        }

        Ok(Translation {
            paddr: entry.paddr | (vaddr & page_mask),
            cache: entry.cache,
        })
    }

    /// See CpuM68k::inspect_translate()
    pub(in crate::cpu_m68k) fn mmu040_inspect_translate(
        &mut self,
        fc: u8,
        vaddr: Address,
    ) -> Option<Address>
    where
        TBus: InspectableBus<Address, Byte>,
    {
        if self.mmu040_tt_match(fc, vaddr).is_some() || !self.mmu040_enabled() {
            return Some(vaddr);
        }

        let page_shift = self.mmu040_page_shift();
        let page_mask = (1 << page_shift) - 1;
        let tag = Atc040::tag(vaddr, page_mask, fc & (1 << 2) != 0);
        if let Some(entry) = self.atc040[Self::atc040_index(fc)].peek(tag, page_shift) {
            // An access through an entry that isn't resident faults
            return entry.resident.then_some(entry.paddr | (vaddr & page_mask));
        }
        self.mmu040_inspect_walk(fc, vaddr)
    }

    /// Walks the page tables for one address as mmu040_translate_lookup()
    /// does, without setting any U bit
    fn mmu040_inspect_walk(&mut self, fc: u8, vaddr: Address) -> Option<Address>
    where
        TBus: InspectableBus<Address, Byte>,
    {
        let walk = self.mmu040_walk(fc, vaddr, |cpu, addr, _| {
            cpu.pmmu_inspect_descriptor(addr)
                .ok_or_else(|| anyhow!("Translation table can't be read"))
        });
        walk.ok().flatten().map(|walk| walk.paddr)
    }
}
