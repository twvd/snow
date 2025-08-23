use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::{CpuError, CpuM68k, Group0Details, HistoryEntry};
use crate::cpu_m68k::pmmu::regs::{PmmuPageDescriptorType, RegisterPSR, RootPointerReg};
use crate::cpu_m68k::CpuM68kType;
use crate::types::Long;

use anyhow::{anyhow, bail, Result};
use arrayvec::ArrayVec;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;

/// Index in CpuM68k::pmmu_atc tables when URP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_URP: usize = 0;
/// Index in CpuM68k::pmmu_atc tables when SRP is in use
pub(in crate::cpu_m68k) const PMMU_ATC_SRP: usize = 1;

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

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    pub(in crate::cpu_m68k) fn pmmu_cache_invalidate(&mut self) {
        if !self.regs.pmmu.tc.enable() {
            return;
        }

        let cache_size =
            (Address::MAX >> (self.regs.pmmu.tc.is() + self.regs.pmmu.tc.ps() as Address)) as usize;
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
    ) -> Result<Address> {
        if dt != PmmuPageDescriptorType::Valid4b {
            bail!("Unimplemented DT {:?}", dt);
        }
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
                bail!(CpuError::Pagefault);
            }
            PmmuPageDescriptorType::PageDescriptor => {
                // Done
                // TODO page size??
                let entry = PmmuShortPageDescriptor(entry_word);
                // TODO protection
                if tis.len() <= 2 {
                    //log::debug!("level {} entry {:?}", tis.len(), entry);
                }
                Ok(entry.page_addr() << 8)
            }
            PmmuPageDescriptorType::Valid4b => {
                // Recurse to child
                let entry = PmmuShortTableDescriptor(entry_word);
                self.pmmu_fetch_table(vaddr << ti, entry.table_addr() << 4, dt, tis, used_bits)
            }
            PmmuPageDescriptorType::Valid8b => todo!(),
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
        if let Some(cached_paddr) = self.pmmu_atc[atc][cache_key] {
            return Ok(cached_paddr | (vaddr & page_mask));
        }

        self.pmmu_translate_lookup::<false>(fc, vaddr, writing)
            .inspect(|paddr| {
                let cache_key = ((vaddr & !is_mask) >> self.regs.pmmu.tc.ps()) as usize;
                self.pmmu_atc[atc][cache_key] = Some(*paddr & !page_mask);
            })
    }

    /// Perform address translation by performing a page table lookup
    pub(in crate::cpu_m68k) fn pmmu_translate_lookup<const PTEST: bool>(
        &mut self,
        fc: u8,
        vaddr: Address,
        writing: bool,
    ) -> Result<Address> {
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
        let page_addr = self
            .pmmu_fetch_table(
                vaddr << self.regs.pmmu.tc.is(),
                rootptr.table_addr() << 4,
                PmmuPageDescriptorType::from_u8(rootptr.dt()).unwrap(),
                &mut tis,
                &mut used_bits,
            )
            .map_err(|e| match e.downcast_ref() {
                Some(CpuError::AddressError(ae)) => {
                    anyhow!("Address error while reading page tables: {:X?}", ae)
                }
                Some(CpuError::Pagefault) => {
                    if PTEST {
                        self.regs.pmmu.psr.set_invalid(true);
                        self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
                    } else if self.history_enabled {
                        self.history.push_back(HistoryEntry::Pagefault {
                            address: vaddr,
                            write: writing,
                        });
                    }

                    anyhow!(CpuError::BusError(Group0Details {
                        function_code: fc,
                        ir: 0,

                        instruction: false,
                        read: !writing,
                        address: vaddr,

                        // Filled in later
                        start_pc: 0,
                        size: 0,
                    }))
                }
                _ => e,
            })?;

        let mask = 0xFFFFFFFFu32.unbounded_shr(used_bits);
        let paddr = (page_addr & !mask) | (vaddr & mask);

        if PTEST {
            self.regs.pmmu.psr.set_level_number((4 - tis.len()) as u8);
        }
        //if tis.len() <= 2 && paddr != vaddr {
        //    log::debug!("{:02X?}", self.regs.pmmu);
        //    log::debug!(
        //        "page_addr {:08X} mask {:08X} ub {}",
        //        page_addr,
        //        mask,
        //        used_bits
        //    );
        //    log::debug!("{:08X} -> {:08X}", paddr, vaddr);
        //}
        Ok(paddr)
    }
}
