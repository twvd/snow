//! M68040 MMU - address translation
//!
//! Only supports 3 fixed levels of tables and 4K/8K page sizes
//! This module only contains the (much simpler) 68040 MMU logic,
//! which hooks into the PMMU logic.
//! Sorry if this is messy/hard to follow

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::{CpuError, CpuM68k, PagefaultCause};
use crate::cpu_m68k::pmmu::translate::{PMMU_ATC_SRP, PMMU_ATC_URP, PmmuAtcEntry};
use crate::cpu_m68k::{CpuM68kType, FpuM68kType, M68040};
use crate::types::Long;

use anyhow::{Result, bail};

const DESC_TYPE: Long = 0b11;
const DESC_TYPE_INVALID: Long = 0;
const DESC_TYPE_INDIRECT: Long = 2;
const DESC_WP: Long = 1 << 2;
const DESC_U: Long = 1 << 3;
const DESC_M: Long = 1 << 4;
const DESC_S: Long = 1 << 7;

const TC_ENABLE: Long = 1 << 15;
const TC_PAGESIZE_8K: Long = 1 << 14;

const MMUSR_S: Long = 1 << 7;
const MMUSR_W: Long = 1 << 2;
const MMUSR_T: Long = 1 << 1;
const MMUSR_R: Long = 1 << 0;

const TT_ENABLE: Long = 1 << 15;
const TT_WP: Long = 1 << 2;

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
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

    /// Returns true if any enabled TT register transparently maps this access.
    /// TT regions bypass the page tables and the ATC entirely.
    ///
    /// Returns None if transparent translation is disabled.
    fn mmu040_tt_match(&self, fc: u8, vaddr: Address) -> Option<bool> {
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

            return Some(tt & TT_WP != 0);
        }

        None
    }

    /// Reads a descriptor and sets its U bit
    fn mmu040_read_descriptor(&mut self, addr: Address) -> Result<Long> {
        let desc = self.read_ticks_physical::<Long>(addr)?;
        if desc & DESC_TYPE != DESC_TYPE_INVALID && desc & DESC_U == 0 {
            self.write_ticks_physical::<Long>(addr, desc | DESC_U)?;
        }
        Ok(desc)
    }

    /// Walks the page tables for one address.
    ///
    /// Try to keep return type in sync with pmmu_translate_lookup
    pub(in crate::cpu_m68k) fn mmu040_translate_lookup(
        &mut self,
        fc: u8,
        vaddr: Address,
    ) -> Result<(Address, bool, bool, Address, bool)> {
        // TODO change return tuple to struct
        debug_assert_eq!(CPU_TYPE, M68040);

        let root_ptr = if fc & (1 << 2) != 0 {
            self.regs.mmu040.srp
        } else {
            self.regs.mmu040.urp
        };

        // Level 1: root table (128 entries)
        let root_addr = root_ptr.wrapping_add(((vaddr >> 25) & 0x7F) * 4);
        let root_desc = self.mmu040_read_descriptor(root_addr)?;
        if root_desc & DESC_TYPE == DESC_TYPE_INVALID {
            bail!(CpuError::Pagefault(PagefaultCause::Invalid));
        }

        // Level 2: pointer table (128 entries, 512 byte aligned)
        let ptr_addr = (root_desc & !0x1FF).wrapping_add(((vaddr >> 18) & 0x7F) * 4);
        let ptr_desc = self.mmu040_read_descriptor(ptr_addr)?;
        if ptr_desc & DESC_TYPE == DESC_TYPE_INVALID {
            bail!(CpuError::Pagefault(PagefaultCause::Invalid));
        }

        // Level 3: page table (64 entries for 4KB pages, 32 for 8KB)
        let (page_shift, table_mask) = if self.regs.mmu040.tc & TC_PAGESIZE_8K != 0 {
            (13, !0x7F)
        } else {
            (12, !0xFF)
        };
        let page_idx = (vaddr >> page_shift) & if page_shift == 13 { 0x1F } else { 0x3F };
        let mut leaf_addr = (ptr_desc & table_mask).wrapping_add(page_idx * 4);
        let mut leaf_desc = self.mmu040_read_descriptor(leaf_addr)?;

        // A leaf descriptor may point at another descriptor. Exactly one level
        // of indirection is allowed; anything deeper is invalid.
        if leaf_desc & DESC_TYPE == DESC_TYPE_INDIRECT {
            leaf_addr = leaf_desc & !DESC_TYPE;
            leaf_desc = self.mmu040_read_descriptor(leaf_addr)?;
            if leaf_desc & DESC_TYPE == DESC_TYPE_INDIRECT {
                bail!(CpuError::Pagefault(PagefaultCause::Invalid));
            }
        }
        if leaf_desc & DESC_TYPE == DESC_TYPE_INVALID {
            bail!(CpuError::Pagefault(PagefaultCause::Invalid));
        }

        // Write protection is inherited from every level of the walk
        let wp = (root_desc | ptr_desc | leaf_desc) & DESC_WP != 0;
        let page_mask = (1 << page_shift) - 1;

        Ok((
            (leaf_desc & !page_mask) | (vaddr & page_mask),
            wp,
            leaf_desc & DESC_S != 0,
            leaf_addr,
            leaf_desc & DESC_M != 0,
        ))
    }

    pub(in crate::cpu_m68k) fn mmu040_ptest(&mut self, fc: u8, vaddr: Address) -> Result<()> {
        // A transparent translation hit reports the logical address as the
        // physical one and no attributes beyond T/R and write protection.
        if let Some(wp) = self.mmu040_tt_match(fc, vaddr) {
            self.regs.mmu040.mmusr =
                (vaddr & !0xFFF) | MMUSR_T | MMUSR_R | if wp { MMUSR_W } else { 0 };
            return Ok(());
        }

        if !self.mmu040_enabled() {
            self.regs.mmu040.mmusr = (vaddr & !0xFFF) | MMUSR_R;
            return Ok(());
        }

        // PTEST skips the ATC and always walks the tables
        match self.mmu040_translate_lookup(fc, vaddr) {
            Ok((paddr, wp, s, leaf_addr, _)) => {
                let desc = self.read_ticks_physical::<Long>(leaf_addr)?;

                // Bits 10-4 of the leaf descriptor (G, U1/U0, S, CM, M) sit in
                // the same places in MMUSR. Write protection accumulates over
                // the whole walk, so it comes from the lookup instead.
                let mut mmusr = (paddr & !0xFFF) | (desc & 0x0000_07F0) | MMUSR_R;
                if wp {
                    mmusr |= MMUSR_W;
                }
                if s {
                    mmusr |= MMUSR_S;
                }
                self.regs.mmu040.mmusr = mmusr;
            }
            Err(e) => match e.downcast_ref() {
                // Lookup failed
                Some(CpuError::Pagefault(_)) => self.regs.mmu040.mmusr = 0,
                _ => return Err(e),
            },
        }

        Ok(())
    }

    /// Translates a logical to a physical address
    pub(in crate::cpu_m68k) fn mmu040_translate(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<Address> {
        debug_assert_eq!(CPU_TYPE, M68040);

        // Transparent translation is in effect even when paging is disabled
        if let Some(wp) = self.mmu040_tt_match(fc, vaddr) {
            if writing && wp {
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
            return Ok(vaddr);
        }

        if !self.mmu040_enabled() {
            return Ok(vaddr);
        }

        let supervisor = fc & (1 << 2) != 0;
        let atc = if supervisor {
            PMMU_ATC_SRP
        } else {
            PMMU_ATC_URP
        };
        let page_shift = self.mmu040_page_shift();
        let page_mask = (1u32 << page_shift) - 1;
        let cache_key = (vaddr >> page_shift) as usize;

        if let Some(entry) = self.pmmu_atc_lookup(atc, cache_key) {
            if (!supervisor && entry.s) || (writing && entry.wp) {
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
            if writing && !entry.modified {
                // Set M-bit
                let desc = self.read_ticks_physical::<Long>(entry.leaf_desc_addr)?;
                self.write_ticks_physical::<Long>(entry.leaf_desc_addr, desc | DESC_M)?;
                self.pmmu_atc[atc][cache_key] = Some(PmmuAtcEntry {
                    modified: true,
                    ..entry
                });
            }
            return Ok(entry.paddr | (vaddr & page_mask));
        }

        let (paddr, wp, s, leaf_desc_addr, modified) = self
            .mmu040_translate_lookup(fc, vaddr)
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::Pagefault(_)) => {
                    self.pmmu_record_pagefault(vaddr, writing);
                    Self::pmmu_pagefault_to_buserror(fc, vaddr, writing)
                }
                _ => e,
            })?;

        if (!supervisor && s) || (writing && wp) {
            self.pmmu_record_pagefault(vaddr, writing);
            return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
        }

        let modified = if writing && !modified {
            let desc = self.read_ticks_physical::<Long>(leaf_desc_addr)?;
            self.write_ticks_physical::<Long>(leaf_desc_addr, desc | DESC_M)?;
            true
        } else {
            modified
        };

        self.pmmu_atc[atc][cache_key] = Some(PmmuAtcEntry {
            paddr: paddr & !page_mask,
            wp,
            s,
            leaf_desc_addr,
            modified,
            generation: self.pmmu_atc_generation,
        });

        Ok(paddr)
    }
}
