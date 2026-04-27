use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::cpu::{CpuError, CpuM68k, Group0Details, HistoryEntry, PagefaultCause};
use crate::cpu_m68k::pmmu::regs::{PmmuPageDescriptorType, RegisterPSR, RootPointerReg};
use crate::types::Long;

use anyhow::{Result, anyhow, bail};
use arrayvec::ArrayVec;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;

/// Index in CpuM68k::pmmu_atc tables when URP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_URP: usize = 0;
/// Index in CpuM68k::pmmu_atc tables when SRP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_SRP: usize = 1;

/// A resolved Address Translation Cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cpu_m68k) struct PmmuAtcEntry {
    /// Physical page base address (low PS bits are zero).
    pub paddr: Address,
    /// Write-protect bit inherited from any table descriptor on the walk
    /// or from the leaf page descriptor.
    pub wp: bool,
}

bitfield! {
    /// Short format page descriptor
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct PmmuShortPageDescriptor(pub u32): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Page address (physical address)
        pub page_addr: u32 @ 8..=31,

        pub dt: u8 @ 0..=1,
        pub wp: bool @ 2,
        pub u: bool @ 3,
        pub m: bool @ 4,
        pub l: bool @ 5,
        pub ci: bool @ 6,
        pub g: bool @ 7,
    }
}

bitfield! {
    /// Long format page descriptor (type 1 and 2)
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct PmmuLongPageDescriptor(pub u64): Debug, FromStorage, IntoStorage, DerefStorage {
        pub lsl: u32 @ 0..=31,
        pub msl: u32 @ 32..=63,

        /// Page address (physical address)
        pub page_addr: u32 @ 8..=31,

        pub dt: u8 @ 32..=33,
        pub wp: bool @ 34,
        pub u: bool @ 35,
        pub m: bool @ 36,
        pub l: bool @ 37,
        pub ci: bool @ 38,
        pub g: bool @ 39,
        pub s: bool @ 40,
        pub sg: bool @ 41,
        pub wal: u8 @ 42..=44,
        pub ral: u8 @ 45..=47,
        pub limit: u8 @ 56..=62,
        pub lu: bool @ 63,
    }
}

bitfield! {
    /// Short format table descriptor
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct PmmuShortTableDescriptor(pub u32): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Table address (physical address)
        pub table_addr: u32 @ 4..=31,

        pub dt: u8 @ 0..=1,
        pub wp: bool @ 2,
        pub u: bool @ 3,
    }
}

bitfield! {
    /// Long format table descriptor
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct PmmuLongTableDescriptor(pub u64): Debug, FromStorage, IntoStorage, DerefStorage {
        pub lsl: u32 @ 0..=31,
        pub msl: u32 @ 32..=63,

        /// Table address (physical address)
        pub table_addr: u32 @ 4..=31,

        pub dt: u8 @ 32..=33,
        pub wp: bool @ 34,
        pub u: bool @ 35,
        pub s: bool @ 40,
        pub sg: bool @ 41,
        pub wal: u8 @ 42..=44,
        pub ral: u8 @ 45..=47,
        pub limit: u8 @ 56..=62,
        pub lu: bool @ 63,
    }
}

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
    pub(in crate::cpu_m68k) fn pmmu_cache_invalidate(&mut self) {
        if !self.regs.pmmu.tc.enable() {
            return;
        }

        let cache_size =
            (Address::MAX >> (self.regs.pmmu.tc.is() + self.regs.pmmu.tc.ps() as Address)) as usize
                + 1;
        if self.pmmu_atc.iter().map(|atc| atc.len()).min().unwrap() < cache_size {
            log::debug!("Expanding cache size: {}", cache_size);
            self.pmmu_atc
                .iter_mut()
                .for_each(|atc| atc.resize(cache_size, None));
        }
        self.pmmu_atc.iter_mut().for_each(|atc| atc.fill(None));
    }

    #[inline(always)]
    fn pmmu_rootptr(&self, fc: u8) -> RootPointerReg {
        // M68851 manual 5.1.4.2
        // + Table 3-1, M68000 Family Function Code Assignments
        //
        // FC3 is not output by the 68020 so we ignore DRP here
        if fc & (1 << 2) != 0 && self.regs.pmmu.tc.sre() {
            self.regs.pmmu.srp
        } else {
            self.regs.pmmu.crp
        }
    }

    #[inline(always)]
    fn pmmu_atc_tableidx(&self, fc: u8) -> usize {
        // M68851 manual 5.1.4.2
        // + Table 3-1, M68000 Family Function Code Assignments
        //
        // FC3 is not output by the 68020 so we ignore DRP here
        if fc & (1 << 2) != 0 && self.regs.pmmu.tc.sre() {
            PMMU_ATC_SRP
        } else {
            PMMU_ATC_URP
        }
    }

    fn pmmu_fetch_table(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        dt: PmmuPageDescriptorType,
        tis: &mut ArrayVec<u8, 4>,
        used_bits: &mut Address,
        wp: bool,
    ) -> Result<(Address, bool)> {
        match dt {
            PmmuPageDescriptorType::Valid4b => {
                self.pmmu_fetch_table_short(vaddr, table_addr, tis, used_bits, wp)
            }
            PmmuPageDescriptorType::Valid8b => {
                self.pmmu_fetch_table_long(vaddr, table_addr, tis, used_bits, wp)
            }
            _ => bail!("Unimplemented DT {:?}", dt),
        }
    }

    fn pmmu_fetch_table_short(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        tis: &mut ArrayVec<u8, 4>,
        used_bits: &mut Address,
        wp: bool,
    ) -> Result<(Address, bool)> {
        let Some(ti) = tis.pop() else {
            bail!("PMMU table search beyond maximum depth");
        };
        *used_bits += ti as Address;

        // Table index
        let idx = vaddr >> (32 - ti);
        let entry_addr = table_addr.wrapping_add(idx * 4);

        self.regs.pmmu.last_desc = entry_addr;

        let entry_word = self.read_ticks_physical::<Long>(entry_addr)?;
        let child_dt = PmmuPageDescriptorType::from_u32(entry_word & 0b11).unwrap();
        match child_dt {
            PmmuPageDescriptorType::Invalid => {
                bail!(CpuError::Pagefault(PagefaultCause::Invalid));
            }
            PmmuPageDescriptorType::PageDescriptor => {
                let entry = PmmuShortPageDescriptor(entry_word);
                if entry.l() {
                    bail!(
                        "Unimplemented PMMU bit: short page descriptor L (locked) at {:08X}",
                        entry_addr
                    );
                }
                // CI (Cache Inhibit) bit deliberately ignored (TODO D-cache)
                if entry.g() {
                    bail!(
                        "Unimplemented PMMU bit: short page descriptor G (globally shared) at {:08X}",
                        entry_addr
                    );
                }
                // TODO U/M bits
                Ok((entry.page_addr() << 8, wp | entry.wp()))
            }
            PmmuPageDescriptorType::Valid4b | PmmuPageDescriptorType::Valid8b => {
                // Recurse to child
                let entry = PmmuShortTableDescriptor(entry_word);
                // TODO U-bit
                self.pmmu_fetch_table(
                    vaddr << ti,
                    entry.table_addr() << 4,
                    child_dt,
                    tis,
                    used_bits,
                    // WP is inherited from any ancestor table
                    wp | entry.wp(),
                )
            }
        }
    }

    fn pmmu_fetch_table_long(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        tis: &mut ArrayVec<u8, 4>,
        used_bits: &mut Address,
        wp: bool,
    ) -> Result<(Address, bool)> {
        let Some(ti) = tis.pop() else {
            bail!("PMMU table search beyond maximum depth");
        };
        *used_bits += ti as Address;

        // Table index
        let idx = vaddr >> (32 - ti);
        let entry_addr = table_addr.wrapping_add(idx * 8);

        self.regs.pmmu.last_desc = entry_addr;

        let entry_word1 = self.read_ticks_physical::<Long>(entry_addr)?;
        let entry_word2 = self.read_ticks_physical::<Long>(entry_addr + 4)?;

        let child_dt = PmmuPageDescriptorType::from_u32(entry_word1 & 0b11).unwrap();
        match child_dt {
            PmmuPageDescriptorType::Invalid => {
                bail!(CpuError::Pagefault(PagefaultCause::Invalid));
            }
            PmmuPageDescriptorType::PageDescriptor => {
                let entry = PmmuLongPageDescriptor(0)
                    .with_msl(entry_word1)
                    .with_lsl(entry_word2);
                if entry.l() {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor L (locked) at {:08X}",
                        entry_addr
                    );
                }
                // CI (Cache Inhibit) bit deliberately ignored (TODO D-cache)
                if entry.g() {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor G (globally shared) at {:08X}",
                        entry_addr
                    );
                }
                if entry.s() {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor S (supervisor-only) at {:08X}",
                        entry_addr
                    );
                }
                if entry.sg() {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor SG (shared globally) at {:08X}",
                        entry_addr
                    );
                }
                if entry.wal() != 0 {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor WAL={:03b} (write access level) at {:08X}",
                        entry.wal(),
                        entry_addr
                    );
                }
                if entry.ral() != 0 {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor RAL={:03b} (read access level) at {:08X}",
                        entry.ral(),
                        entry_addr
                    );
                }
                if (!entry.lu() && entry.limit() < 0x7F) || (entry.lu() && entry.limit() > 0) {
                    bail!(
                        "Unimplemented PMMU bit: long page descriptor LIMIT={} LU={} at {:08X}",
                        entry.limit(),
                        entry.lu(),
                        entry_addr
                    );
                }
                // TODO U (used) and M (modified) bits not updated.
                Ok((entry.page_addr() << 8, wp | entry.wp()))
            }
            PmmuPageDescriptorType::Valid4b | PmmuPageDescriptorType::Valid8b => {
                // Recurse to child
                let entry = PmmuLongTableDescriptor(0)
                    .with_msl(entry_word1)
                    .with_lsl(entry_word2);
                if entry.s() {
                    bail!(
                        "Unimplemented PMMU bit: long table descriptor S (supervisor-only) at {:08X}",
                        entry_addr
                    );
                }
                if entry.sg() {
                    bail!(
                        "Unimplemented PMMU bit: long table descriptor SG (shared globally) at {:08X}",
                        entry_addr
                    );
                }
                if entry.wal() != 0 {
                    bail!(
                        "Unimplemented PMMU bit: long table descriptor WAL={:03b} (write access level) at {:08X}",
                        entry.wal(),
                        entry_addr
                    );
                }
                if entry.ral() != 0 {
                    bail!(
                        "Unimplemented PMMU bit: long table descriptor RAL={:03b} (read access level) at {:08X}",
                        entry.ral(),
                        entry_addr
                    );
                }
                if (!entry.lu() && entry.limit() < 0x7F) || (entry.lu() && entry.limit() > 0) {
                    bail!(
                        "Unimplemented PMMU bit: long table descriptor LIMIT={} LU={} at {:08X}",
                        entry.limit(),
                        entry.lu(),
                        entry_addr
                    );
                }
                self.pmmu_fetch_table(
                    vaddr << ti,
                    entry.table_addr() << 4,
                    child_dt,
                    tis,
                    used_bits,
                    // WP is inherited from any ancestor table
                    wp | entry.wp(),
                )
            }
        }
    }

    pub(in crate::cpu_m68k) fn pmmu_translate(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<Address> {
        if !PMMU || !self.regs.pmmu.tc.enable() {
            return Ok(vaddr);
        }

        // This is formally tested in PMOVE when translation is enabled
        debug_assert_eq!(
            self.regs.pmmu.tc.is()
                + self.regs.pmmu.tc.tia() as u32
                + self.regs.pmmu.tc.tib() as u32
                + self.regs.pmmu.tc.tic() as u32
                + self.regs.pmmu.tc.tid() as u32
                + self.regs.pmmu.tc.ps() as u32,
            32
        );

        let atc = self.pmmu_atc_tableidx(fc);
        let is_mask = Address::MAX.unbounded_shl(32 - self.regs.pmmu.tc.is());
        let page_mask = (1u32 << self.regs.pmmu.tc.ps()) - 1;
        let cache_key = ((vaddr & !is_mask) >> self.regs.pmmu.tc.ps()) as usize;
        if let Some(entry) = self.pmmu_atc[atc][cache_key] {
            if writing && entry.wp {
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
            return Ok(entry.paddr | (vaddr & page_mask));
        }

        let (paddr, wp) = self.pmmu_translate_lookup::<false>(fc, vaddr, writing)?;
        let cache_key = ((vaddr & !is_mask) >> self.regs.pmmu.tc.ps()) as usize;
        self.pmmu_atc[atc][cache_key] = Some(PmmuAtcEntry {
            paddr: paddr & !page_mask,
            wp,
        });
        Ok(paddr)
    }

    /// Records a page-fault history entry if history recording is enabled.
    fn pmmu_record_pagefault(&mut self, vaddr: Address, writing: bool) {
        if self.history_enabled {
            self.history.push_back(HistoryEntry::Pagefault {
                address: vaddr,
                write: writing,
            });
        }
    }

    /// Builds the Group-0 BusError stack frame error value for a page fault.
    fn pmmu_pagefault_to_buserror(fc: u8, vaddr: Address, writing: bool) -> anyhow::Error {
        anyhow!(CpuError::BusError(Group0Details {
            function_code: fc,
            ir: 0,
            instruction: false,
            read: !writing,
            address: vaddr,
            start_pc: 0,
            size: 0,
        }))
    }

    /// Perform address translation by performing a page table lookup.
    /// Returns (physical address, wp), or error:
    ///  - bus error stack frame for translation,
    ///  - simple error on PTEST.
    pub(in crate::cpu_m68k) fn pmmu_translate_lookup<const PTEST: bool>(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<(Address, bool)> {
        let rootptr = self.pmmu_rootptr(fc);

        if rootptr.sg() {
            bail!(
                "Unimplemented PMMU bit: root pointer SG (shared globally) set: {:016X}",
                rootptr.0
            );
        }
        // Root pointer LIMIT field is 15 bits (0..=0x7FFF).
        // LU=0 (upper bound): no-op when LIMIT == 0x7FFF.
        // LU=1 (lower bound): no-op when LIMIT == 0.
        if (!rootptr.lu() && rootptr.limit() < 0x7FFF) || (rootptr.lu() && rootptr.limit() > 0) {
            bail!(
                "Unimplemented PMMU bit: root pointer LIMIT={} LU={}",
                rootptr.limit(),
                rootptr.lu()
            );
        }

        if PTEST {
            self.regs.pmmu.psr = RegisterPSR::default();
            self.regs.pmmu.last_desc = 0;
        }

        let mut tis = ArrayVec::from([
            self.regs.pmmu.tc.tid(),
            self.regs.pmmu.tc.tic(),
            self.regs.pmmu.tc.tib(),
            self.regs.pmmu.tc.tia(),
        ]);
        let mut used_bits = self.regs.pmmu.tc.is() as Address;
        let walk = self
            .pmmu_fetch_table(
                vaddr << self.regs.pmmu.tc.is(),
                rootptr.table_addr() << 4,
                PmmuPageDescriptorType::from_u8(rootptr.dt()).unwrap(),
                &mut tis,
                &mut used_bits,
                false,
            )
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::AddressError(ae)) => {
                    anyhow!("Address error while reading page tables: {:X?}", ae)
                }
                Some(CpuError::Pagefault(cause)) => {
                    let cause = *cause;
                    if PTEST {
                        match cause {
                            PagefaultCause::Invalid => self.regs.pmmu.psr.set_invalid(true),
                            PagefaultCause::WriteProtected => {
                                self.regs.pmmu.psr.set_write_protected(true)
                            }
                        }
                        self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
                        // Raise basic error rather than full bus error frame for PTEST
                        e
                    } else {
                        self.pmmu_record_pagefault(vaddr, writing);
                        Self::pmmu_pagefault_to_buserror(fc, vaddr, writing)
                    }
                }
                _ => e,
            });

        let (page_addr, wp) = walk?;

        // Enforce write-protect on the resolved page
        if writing && wp {
            if PTEST {
                self.regs.pmmu.psr.set_write_protected(true);
                self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
                return Err(anyhow!(CpuError::Pagefault(PagefaultCause::WriteProtected)));
            } else {
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
        }

        let mask = 0xFFFFFFFFu32.unbounded_shr(used_bits);
        let paddr = (page_addr & !mask) | (vaddr & mask);

        if PTEST {
            self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
        }
        Ok((paddr, wp))
    }
}
