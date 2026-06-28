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
use serde::{Deserialize, Serialize};

/// Index in CpuM68k::pmmu_atc tables when URP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_URP: usize = 0;
/// Index in CpuM68k::pmmu_atc tables when SRP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_SRP: usize = 1;
/// Number of ATC tables in CpuM68k::pmmu_atc (one per root pointer)
pub(in crate::cpu_m68k) const PMMU_ATCS: usize = 2;

/// Result of a successful page-table walk.
#[derive(Debug, Clone, Copy)]
struct PmmuWalkResult {
    /// Physical page base address shifted to include the in-page offset bits.
    page_addr: Address,
    /// Write-protected (inherited down the walk or set on the leaf).
    wp: bool,
    /// Supervisor-only (inherited from any long-format descriptor on the walk).
    /// Short-format descriptors have no S bit and contribute nothing.
    s: bool,
    /// Physical address of the leaf page descriptor.
    leaf_desc_addr: Address,
    /// Value of the M (modified) bit in the leaf descriptor.
    modified: bool,
}

/// A resolved Address Translation Cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::cpu_m68k) struct PmmuAtcEntry {
    /// Physical page base address (low PS bits are zero).
    pub paddr: Address,
    /// Write-protect bit inherited from any table descriptor on the walk
    /// or from the leaf page descriptor.
    pub wp: bool,
    /// Supervisor-only bit inherited from any long-format descriptor on the walk.
    pub s: bool,
    /// Physical address of the leaf page descriptor (long-word aligned).
    /// Needed so writes can set the descriptor's M (modified) bit without
    /// replaying the full table walk.
    pub leaf_desc_addr: Address,
    /// Whether the M bit of the leaf descriptor is already set. When false,
    /// the next write through this entry must RMW the descriptor to set M.
    pub modified: bool,
}

/// Custom (de)serialization for the PMMU ATC tables.
///
/// ATC is a linear table for O(1) lookup, which can get pretty large in serialized
/// form so this is stored as key/value + size instead.
pub(in crate::cpu_m68k) mod atc_serde {
    use super::{PMMU_ATCS, PmmuAtcEntry};
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct AtcTable {
        size: usize,
        entries: Vec<(usize, PmmuAtcEntry)>,
    }

    impl AtcTable {
        fn from_slots(slots: &[Option<PmmuAtcEntry>]) -> Self {
            Self {
                size: slots.len(),
                entries: slots
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| e.map(|e| (i, e)))
                    .collect(),
            }
        }

        fn into_slots(self) -> Option<Vec<Option<PmmuAtcEntry>>> {
            let mut slots = vec![None; self.size];
            for (i, e) in self.entries {
                *slots.get_mut(i)? = Some(e);
            }
            Some(slots)
        }
    }

    pub(in crate::cpu_m68k) fn serialize<S>(
        atc: &[Vec<Option<PmmuAtcEntry>>; PMMU_ATCS],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let tables = [AtcTable::from_slots(&atc[0]), AtcTable::from_slots(&atc[1])];
        tables.serialize(serializer)
    }

    pub(in crate::cpu_m68k) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<[Vec<Option<PmmuAtcEntry>>; PMMU_ATCS], D::Error>
    where
        D: Deserializer<'de>,
    {
        let [t0, t1] = <[AtcTable; PMMU_ATCS]>::deserialize(deserializer)?;
        let err = || D::Error::custom("ATC entry index out of range for table size");
        Ok([
            t0.into_slots().ok_or_else(err)?,
            t1.into_slots().ok_or_else(err)?,
        ])
    }
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
        pub limit: u16 @ 48..=62,
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
        pub limit: u16 @ 48..=62,
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
    /// Enlarges ATC size if needed by configuration
    pub(in crate::cpu_m68k) fn pmmu_cache_ensure(&mut self) {
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
    }

    /// Flushes complete ATC
    pub(in crate::cpu_m68k) fn pmmu_cache_invalidate(&mut self) {
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

    #[allow(clippy::too_many_arguments)]
    fn pmmu_fetch_table(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        dt: PmmuPageDescriptorType,
        parent_limit: Option<(u16, bool)>,
        tis: &mut ArrayVec<u8, 4>,
        used_bits: &mut Address,
        wp: bool,
        s: bool,
        mutate: bool,
    ) -> Result<PmmuWalkResult> {
        match dt {
            PmmuPageDescriptorType::Valid4b => self.pmmu_fetch_table_short(
                vaddr,
                table_addr,
                parent_limit,
                tis,
                used_bits,
                wp,
                s,
                mutate,
            ),
            PmmuPageDescriptorType::Valid8b => self.pmmu_fetch_table_long(
                vaddr,
                table_addr,
                parent_limit,
                tis,
                used_bits,
                wp,
                s,
                mutate,
            ),
            _ => bail!("Unimplemented DT {:?}", dt),
        }
    }

    fn pmmu_check_limit(idx: Address, parent_limit: Option<(u16, bool)>) -> Result<()> {
        if let Some((limit, lu)) = parent_limit {
            let limit = limit as Address;
            let violation = if lu { idx < limit } else { idx > limit };
            if violation {
                bail!(CpuError::Pagefault(PagefaultCause::LimitViolation));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn pmmu_fetch_table_short(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        parent_limit: Option<(u16, bool)>,
        tis: &mut ArrayVec<u8, 4>,
        used_bits: &mut Address,
        wp: bool,
        s: bool,
        mutate: bool,
    ) -> Result<PmmuWalkResult> {
        let Some(ti) = tis.pop() else {
            bail!("PMMU table search beyond maximum depth");
        };
        *used_bits += ti as Address;

        // Table index
        let idx = vaddr >> (32 - ti);
        Self::pmmu_check_limit(idx, parent_limit)?;
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
                // Mark U on first successful use, so the OS can tell this leaf
                // has been referenced. PTEST must not mutate descriptors.
                if mutate && !entry.u() {
                    self.write_ticks_physical::<Long>(entry_addr, entry_word | (1 << 3))?;
                }
                Ok(PmmuWalkResult {
                    page_addr: entry.page_addr() << 8,
                    wp: wp | entry.wp(),
                    // Short format has no S bit; pass the accumulator through unchanged
                    s,
                    leaf_desc_addr: entry_addr,
                    modified: entry.m(),
                })
            }
            PmmuPageDescriptorType::Valid4b | PmmuPageDescriptorType::Valid8b => {
                // Recurse to child
                let entry = PmmuShortTableDescriptor(entry_word);
                // Set U on the table descriptor we just walked through
                if mutate && !entry.u() {
                    self.write_ticks_physical::<Long>(entry_addr, entry_word | (1 << 3))?;
                }
                self.pmmu_fetch_table(
                    vaddr << ti,
                    entry.table_addr() << 4,
                    child_dt,
                    // Short table descriptors have no limit field
                    None,
                    tis,
                    used_bits,
                    // WP is inherited from any ancestor table
                    wp | entry.wp(),
                    s,
                    mutate,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pmmu_fetch_table_long(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        parent_limit: Option<(u16, bool)>,
        tis: &mut ArrayVec<u8, 4>,
        used_bits: &mut Address,
        wp: bool,
        s: bool,
        mutate: bool,
    ) -> Result<PmmuWalkResult> {
        let Some(ti) = tis.pop() else {
            bail!("PMMU table search beyond maximum depth");
        };
        *used_bits += ti as Address;

        // Table index
        let idx = vaddr >> (32 - ti);
        Self::pmmu_check_limit(idx, parent_limit)?;
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
                // U lives in the MSL (bit 35 of the u64 = bit 3 of MSL), so
                // only the first longword of the descriptor needs rewriting.
                if mutate && !entry.u() {
                    self.write_ticks_physical::<Long>(entry_addr, entry_word1 | (1 << 3))?;
                }
                Ok(PmmuWalkResult {
                    page_addr: entry.page_addr() << 8,
                    wp: wp | entry.wp(),
                    s: s | entry.s(),
                    leaf_desc_addr: entry_addr,
                    modified: entry.m(),
                })
            }
            PmmuPageDescriptorType::Valid4b | PmmuPageDescriptorType::Valid8b => {
                // Recurse to child
                let entry = PmmuLongTableDescriptor(0)
                    .with_msl(entry_word1)
                    .with_lsl(entry_word2);
                if mutate && !entry.u() {
                    self.write_ticks_physical::<Long>(entry_addr, entry_word1 | (1 << 3))?;
                }
                self.pmmu_fetch_table(
                    vaddr << ti,
                    entry.table_addr() << 4,
                    child_dt,
                    // Long table descriptors carry a limit constraining the
                    // child table's index
                    Some((entry.limit(), entry.lu())),
                    tis,
                    used_bits,
                    // WP is inherited from any ancestor table
                    wp | entry.wp(),
                    // S is inherited from any long-format ancestor
                    s | entry.s(),
                    mutate,
                )
            }
        }
    }

    /// Returns true if any enabled TT register transparently maps this access.
    /// TT regions bypass the page tables and the ATC entirely.
    #[inline]
    fn pmmu_tt_match(&self, fc: u8, vaddr: Address, writing: bool) -> bool {
        for tt in &self.regs.pmmu.tt {
            if !tt.e() {
                continue;
            }
            // FC: bits set in fc_mask are don't-cares.
            let fc_care = !tt.fc_mask() & 0b111;
            if (fc & fc_care) != (tt.fc_base() & fc_care) {
                continue;
            }
            // R/W: rwm=1 means R/W is don't-care, otherwise rw must be ok
            if !tt.rwm() && writing != tt.rw() {
                continue;
            }
            // Address (top 3 bytes are don't care)
            let addr_care = !tt.le_mask() & 0xFF;
            if ((vaddr >> 24) & addr_care) != (tt.le_base() & addr_care) {
                continue;
            }
            return true;
        }
        false
    }

    pub(in crate::cpu_m68k) fn pmmu_translate(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<Address> {
        if !PMMU {
            return Ok(vaddr);
        }

        // Transparent translation runs even when TC.E=0; TT regions are
        // identity-mapped and bypass the page tables and the ATC.
        if self.pmmu_tt_match(fc, vaddr, writing) {
            return Ok(vaddr);
        }

        if !self.regs.pmmu.tc.enable() {
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

        let supervisor = fc & (1 << 2) != 0;
        let atc = self.pmmu_atc_tableidx(fc);
        let is_mask = Address::MAX.unbounded_shl(32 - self.regs.pmmu.tc.is());
        let page_mask = (1u32 << self.regs.pmmu.tc.ps()) - 1;
        let cache_key = ((vaddr & !is_mask) >> self.regs.pmmu.tc.ps()) as usize;
        if let Some(entry) = self.pmmu_atc[atc][cache_key] {
            if !supervisor && entry.s {
                self.pmmu_record_atc_fault(vaddr, writing, |psr| {
                    psr.set_supervisor_violation(true);
                });
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
            if writing && entry.wp {
                self.pmmu_record_atc_fault(vaddr, writing, |psr| {
                    psr.set_write_protected(true);
                });
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
            if writing && !entry.modified {
                // First write through an unmodified page: RMW the leaf
                // descriptor to set the M bit, then promote the ATC entry.
                let desc = self.read_ticks_physical::<Long>(entry.leaf_desc_addr)?;
                self.write_ticks_physical::<Long>(entry.leaf_desc_addr, desc | (1 << 4))?;
                self.pmmu_atc[atc][cache_key] = Some(PmmuAtcEntry {
                    modified: true,
                    ..entry
                });
            }
            return Ok(entry.paddr | (vaddr & page_mask));
        }

        let (paddr, wp, s, leaf_desc_addr, modified) =
            self.pmmu_translate_lookup::<false>(fc, vaddr, writing)?;
        let cache_key = ((vaddr & !is_mask) >> self.regs.pmmu.tc.ps()) as usize;
        self.pmmu_atc[atc][cache_key] = Some(PmmuAtcEntry {
            paddr: paddr & !page_mask,
            wp,
            s,
            leaf_desc_addr,
            modified,
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

    /// Set PSR for a failure during ATC lookup
    fn pmmu_record_atc_fault(
        &mut self,
        vaddr: Address,
        writing: bool,
        set_cause: impl FnOnce(&mut RegisterPSR),
    ) {
        let leaf_level = [
            self.regs.pmmu.tc.tia(),
            self.regs.pmmu.tc.tib(),
            self.regs.pmmu.tc.tic(),
            self.regs.pmmu.tc.tid(),
        ]
        .iter()
        .filter(|&&x| x > 0)
        .count() as u8;
        self.regs.pmmu.psr = RegisterPSR::default();
        set_cause(&mut self.regs.pmmu.psr);
        self.regs.pmmu.psr.set_level_number(leaf_level);
        self.regs.pmmu.psr.set_bus_error(true);
        self.pmmu_record_pagefault(vaddr, writing);
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
    /// Returns (physical address, wp, s, leaf descriptor address, M-bit), or error:
    ///  - bus error stack frame for translation,
    ///  - simple error on PTEST.
    pub(in crate::cpu_m68k) fn pmmu_translate_lookup<const PTEST: bool>(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<(Address, bool, bool, Address, bool)> {
        let rootptr = self.pmmu_rootptr(fc);

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
                // Root pointer always carries a limit (15 bits, LU bit)
                Some((rootptr.limit(), rootptr.lu())),
                &mut tis,
                &mut used_bits,
                false,
                false,
                !PTEST,
            )
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::AddressError(ae)) => {
                    anyhow!("Address error while reading page tables: {:X?}", ae)
                }
                Some(CpuError::Pagefault(cause)) => {
                    let cause = *cause;
                    if !PTEST {
                        self.regs.pmmu.psr = RegisterPSR::default();
                    }
                    match cause {
                        PagefaultCause::Invalid => self.regs.pmmu.psr.set_invalid(true),
                        PagefaultCause::WriteProtected => {
                            self.regs.pmmu.psr.set_write_protected(true);
                        }
                        PagefaultCause::SupervisorOnly => {
                            self.regs.pmmu.psr.set_supervisor_violation(true);
                        }
                        PagefaultCause::LimitViolation => {
                            self.regs.pmmu.psr.set_limit_violation(true);
                        }
                    }
                    self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
                    if PTEST {
                        // Raise basic error rather than full bus error frame for PTEST
                        e
                    } else {
                        self.regs.pmmu.psr.set_bus_error(true);
                        self.pmmu_record_pagefault(vaddr, writing);
                        Self::pmmu_pagefault_to_buserror(fc, vaddr, writing)
                    }
                }
                _ => e,
            });

        let PmmuWalkResult {
            page_addr,
            wp,
            s,
            leaf_desc_addr,
            mut modified,
        } = walk?;

        // Enforce supervisor-only access on the resolved page
        let supervisor = fc & (1 << 2) != 0;
        if !supervisor && s {
            if !PTEST {
                self.regs.pmmu.psr = RegisterPSR::default();
            }
            self.regs.pmmu.psr.set_supervisor_violation(true);
            self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
            if PTEST {
                return Err(anyhow!(CpuError::Pagefault(PagefaultCause::SupervisorOnly)));
            } else {
                self.regs.pmmu.psr.set_bus_error(true);
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
        }

        // Enforce write-protect on the resolved page
        if writing && wp {
            if !PTEST {
                self.regs.pmmu.psr = RegisterPSR::default();
            }
            self.regs.pmmu.psr.set_write_protected(true);
            self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
            if PTEST {
                return Err(anyhow!(CpuError::Pagefault(PagefaultCause::WriteProtected)));
            } else {
                self.regs.pmmu.psr.set_bus_error(true);
                self.pmmu_record_pagefault(vaddr, writing);
                return Err(Self::pmmu_pagefault_to_buserror(fc, vaddr, writing));
            }
        }

        // Set M on the leaf descriptor on first write. PTEST never mutates the
        // tables; only real translations do.
        if !PTEST && writing && !modified {
            let desc = self.read_ticks_physical::<Long>(leaf_desc_addr)?;
            self.write_ticks_physical::<Long>(leaf_desc_addr, desc | (1 << 4))?;
            modified = true;
        }

        let mask = 0xFFFFFFFFu32.unbounded_shr(used_bits);
        let paddr = (page_addr & !mask) | (vaddr & mask);

        if PTEST {
            self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
        }
        Ok((paddr, wp, s, leaf_desc_addr, modified))
    }
}

#[cfg(all(test, feature = "savestates"))]
mod tests {
    use super::*;

    fn sample_entry(paddr: Address) -> PmmuAtcEntry {
        PmmuAtcEntry {
            paddr,
            wp: true,
            s: false,
            leaf_desc_addr: paddr + 0x10,
            modified: true,
        }
    }

    #[test]
    fn atc_serde_roundtrip() {
        let mut table0 = vec![None; 1024];
        table0[3] = Some(sample_entry(0x1000));
        table0[1000] = Some(sample_entry(0x2000));
        let mut table1 = vec![None; 64];
        table1[0] = Some(sample_entry(0x3000));

        let atc = [table0, table1];

        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrap {
            #[serde(with = "super::atc_serde")]
            atc: [Vec<Option<PmmuAtcEntry>>; PMMU_ATCS],
        }

        let bytes = postcard::to_allocvec(&Wrap { atc: atc.clone() }).unwrap();
        let restored: Wrap = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(restored.atc[0].len(), 1024);
        assert_eq!(restored.atc[1].len(), 64);
        assert_eq!(restored.atc, atc);
    }
}
