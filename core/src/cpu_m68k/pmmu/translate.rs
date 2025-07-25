use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::pmmu::regs::{PmmuPageDescriptorType, RootPointerReg};
use crate::cpu_m68k::CpuM68kType;

use anyhow::{bail, Result};
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;

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

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    fn pmmu_rootptr(&self) -> &RootPointerReg {
        // TODO FC?
        if self.regs.pmmu.tc.sre() {
            todo!();
        }

        &self.regs.pmmu.crp
    }

    fn pmmu_fetch_table(
        &mut self,
        vaddr: Address,
        table_addr: Address,
        dt: PmmuPageDescriptorType,
        ti: u8,
    ) -> Result<Address> {
        if dt != PmmuPageDescriptorType::Valid4b {
            bail!("Unimplemented DT {:?}", dt);
        }

        // Table index
        let idx = vaddr >> (32 - ti);
        let entry_addr = table_addr.wrapping_add(idx * 4);

        let entry = PmmuShortPageDescriptor(self.read_ticks_physical(entry_addr)?);
        if entry.dt() != 1 {
            bail!("TODO descriptor type {}", entry.dt());
        }

        Ok(entry.page_addr() << 8)
    }

    pub(in crate::cpu_m68k) fn pmmu_translate(
        &mut self,
        vaddr: Address,
        _writing: bool,
    ) -> Result<Address> {
        if !PMMU || !self.regs.pmmu.tc.enable() {
            return Ok(vaddr);
        }

        let rootptr = self.pmmu_rootptr();
        let page_addr = self.pmmu_fetch_table(
            vaddr << self.regs.pmmu.tc.is(),
            rootptr.table_addr() << 4,
            PmmuPageDescriptorType::from_u8(rootptr.dt()).unwrap(),
            self.regs.pmmu.tc.tia(),
        )?;

        let used_bits = self.regs.pmmu.tc.is() as u32 + self.regs.pmmu.tc.tia() as u32;
        let mask = 0xFFFFFFFF >> used_bits;
        let paddr = (page_addr & !mask) | (vaddr & mask);
        //log::debug!("{:08X} -> {:08X}", paddr, vaddr);
        Ok(paddr)
    }
}
