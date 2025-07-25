//! M68851 PMMU - Opcode implementations

use anyhow::{bail, Result};

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::{CpuM68k, ExceptionGroup, VECTOR_PRIVILEGE_VIOLATION};
use crate::cpu_m68k::instruction::Instruction;
use crate::cpu_m68k::CpuM68kType;
use crate::types::{DoubleLong, Word};

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

        if extword & 0b1110_0001_0000_0000 == 0b0010_0000_0000_0000 {
            // PFLUSH
            bail!("PFLUSH");
        } else if extword == 0b1010_0000_0000_0000 {
            // PFLUSHR
            bail!("PFLUSHR");
        } else if extword & 0b1110_0001_1111_1111 == 0b0100_0000_0000_0000 {
            // PMOVE (format 1)
            self.op_pmove1(instr, extword)
        } else if extword & 0b1110_0001_1110_0011 == 0b0110_0000_0000_0000 {
            // PMOVE (format 2)
            bail!("PMOVE2");
        } else if extword & 0b1110_0011_1111_1111 == 0b0110_0000_0000_0000 {
            // PMOVE (format 3)
            bail!("PMOVE3");
        } else if extword & 0b1110_0000_0000_0000 == 0b1000_0000_0000_0000 {
            // PTEST
            bail!("PTEST");
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
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        let extword = Pmove1Extword(extword);

        match (extword.preg(), extword.write()) {
            (0b000, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.tc.0)?;
            }
            (0b000, false) => {
                self.regs.pmmu.tc.0 = self.read_ea(instr, instr.get_op2())?;
            }
            (0b001, true) => {
                self.write_ea_sz::<8>(instr, instr.get_op2(), self.regs.pmmu.drp.0.to_be_bytes())?;
            }
            (0b001, false) => {
                self.regs.pmmu.drp.0 =
                    DoubleLong::from_be_bytes(self.read_ea_sz::<8>(instr, instr.get_op2())?);
            }
            (0b010, true) => {
                self.write_ea_sz::<8>(instr, instr.get_op2(), self.regs.pmmu.srp.0.to_be_bytes())?;
            }
            (0b010, false) => {
                self.regs.pmmu.srp.0 =
                    DoubleLong::from_be_bytes(self.read_ea_sz::<8>(instr, instr.get_op2())?);
            }
            (0b011, true) => {
                self.write_ea_sz::<8>(instr, instr.get_op2(), self.regs.pmmu.crp.0.to_be_bytes())?;
            }
            (0b011, false) => {
                self.regs.pmmu.crp.0 =
                    DoubleLong::from_be_bytes(self.read_ea_sz::<8>(instr, instr.get_op2())?);
            }
            (0b100, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.cal.0)?;
            }
            (0b100, false) => {
                self.regs.pmmu.cal.0 = self.read_ea(instr, instr.get_op2())?;
            }
            (0b101, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.val.0)?;
            }
            (0b101, false) => {
                self.regs.pmmu.val.0 = self.read_ea(instr, instr.get_op2())?;
            }
            (0b110, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.scc)?;
            }
            (0b110, false) => {
                self.regs.pmmu.scc = self.read_ea(instr, instr.get_op2())?;
            }
            (0b111, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.ac.0)?;
            }
            (0b111, false) => {
                self.regs.pmmu.ac.0 = self.read_ea(instr, instr.get_op2())?;
            }
            _ => unreachable!(),
        }

        Ok(())
    }
}
