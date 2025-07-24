use anyhow::{bail, Result};

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::{cpu::CpuM68k, instruction::Instruction, CpuM68kType};
use crate::types::Word;

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

        if extword & 0b1110_0001_0000_0000 == 0b0010_0000_0000_0000 {
            // PFLUSH
            log::debug!("PFLUSH");
        } else if extword == 0b1010_0000_0000_0000 {
            // PFLUSHR
            log::debug!("PFLUSHR");
        } else if extword & 0b1110_0001_1111_1111 == 0b0100_0000_0000_0000 {
            // PMOVE (format 1)
            log::debug!("PMOVE1");
        } else if extword & 0b1110_0001_1110_0011 == 0b0110_0000_0000_0000 {
            // PMOVE (format 2)
            log::debug!("PMOVE2");
        } else if extword & 0b1110_0011_1111_1111 == 0b0110_0000_0000_0000 {
            // PMOVE (format 3)
            log::debug!("PMOVE3");
        } else if extword & 0b1110_0000_0000_0000 == 0b1000_0000_0000_0000 {
            // PTEST
            log::debug!("PTEST");
        } else {
            // Unknown instruction
            bail!(
                "Unimplemented PMMU op 000: {:016b} {:016b}",
                instr.data,
                extword
            );
        }

        self.breakpoint_hit.set();

        Ok(())
    }
}
