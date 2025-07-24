use anyhow::{bail, Result};

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::instruction::Instruction;
use crate::cpu_m68k::CpuM68kType;
use crate::types::Word;

use super::instruction::Pmove1Extword;

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    pub(in crate::cpu_m68k) fn op_pop_000(&mut self, instr: &Instruction) -> Result<()> {
        if !PMMU {
            return self.op_linef(instr);
        }

        let extword = self.fetch_immediate::<Word>()?;

        self.breakpoint_hit.set();

        if extword & 0b1110_0001_0000_0000 == 0b0010_0000_0000_0000 {
            // PFLUSH
            log::debug!("PFLUSH");
            Ok(())
        } else if extword == 0b1010_0000_0000_0000 {
            // PFLUSHR
            log::debug!("PFLUSHR");
            Ok(())
        } else if extword & 0b1110_0001_1111_1111 == 0b0100_0000_0000_0000 {
            // PMOVE (format 1)
            self.op_pmove1(instr, extword)
        } else if extword & 0b1110_0001_1110_0011 == 0b0110_0000_0000_0000 {
            // PMOVE (format 2)
            log::debug!("PMOVE2");
            Ok(())
        } else if extword & 0b1110_0011_1111_1111 == 0b0110_0000_0000_0000 {
            // PMOVE (format 3)
            log::debug!("PMOVE3");
            Ok(())
        } else if extword & 0b1110_0000_0000_0000 == 0b1000_0000_0000_0000 {
            // PTEST
            log::debug!("PTEST");
            Ok(())
        } else {
            // Unknown instruction
            bail!(
                "Unimplemented PMMU op 000: {:016b} {:016b}",
                instr.data,
                extword
            );
        }
    }

    pub(in crate::cpu_m68k) fn op_pmove1(
        &mut self,
        instr: &Instruction,
        extword: Word,
    ) -> Result<()> {
        let extword = Pmove1Extword(extword);

        log::debug!("PMOVE1 {:?}", extword);
        Ok(())
    }
}
