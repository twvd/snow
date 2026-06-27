//! M68851 PMMU - Opcode implementations

use crate::cpu_m68k::{FpuM68kType, M68030};
use anyhow::{Result, bail};

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::FC_MASK;
use crate::cpu_m68k::cpu::{CpuM68k, ExceptionGroup, VECTOR_PRIVILEGE_VIOLATION};
use crate::cpu_m68k::instruction::Instruction;
use crate::cpu_m68k::pmmu::instruction::{Pmove3Extword, PtestExtword};
use crate::types::{DoubleLong, Long, Word};

use super::instruction::Pmove1Extword;
use super::regs::{TcReg, TrTranslationReg};

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
    pub(in crate::cpu_m68k) fn op_pop_000(&mut self, instr: &Instruction) -> Result<()> {
        if !PMMU {
            return self.op_linef(instr);
        }

        let extword = self.fetch_immediate::<Word>()?;

        // Mind the ordering here!
        if extword & 0b1111_1101_1110_0000 == 0b0010_0000_0000_0000 {
            // PLOAD
            self.op_pload(instr, extword)
        } else if extword & 0b1110_0011_0000_0000 == 0b0010_0000_0000_0000 {
            // PFLUSH
            self.pmmu_cache_invalidate();
            Ok(())
        } else if extword == 0b1010_0000_0000_0000 {
            // PFLUSHR
            // TODO specific flush
            self.read_ea_sz::<8>(instr, instr.get_op2())?;
            self.pmmu_cache_invalidate();
            Ok(())
        } else if extword & 0b1110_0000_1111_1111 == 0b0100_0000_0000_0000 && CPU_TYPE >= M68030 {
            // PMOVE 68030 version
            self.op_pmove_68030(instr, extword)
        } else if extword & 0b1111_1000_1111_1111 == 0b0000_1000_0000_0000 && CPU_TYPE >= M68030 {
            // PMOVE TT regs (68030+)
            // preg field: 010 = TT0, 011 = TT1; bit 0 of preg picks the register
            let extword = Pmove1Extword(extword);
            if !self.regs.sr.supervisor() {
                self.advance_cycles(4)?;
                return self.raise_exception(
                    ExceptionGroup::Group2,
                    VECTOR_PRIVILEGE_VIOLATION,
                    None,
                );
            }
            let tt_idx = extword.preg() & 0b1;
            if extword.write() {
                // Register to EA
                self.write_ea::<Long>(instr, instr.get_op2(), self.regs.pmmu.tt[tt_idx].0)?;
            } else {
                // EA to register
                let val = self.read_ea::<Long>(instr, instr.get_op2())?;
                self.regs.pmmu.tt[tt_idx] = TrTranslationReg(val);
                // TT changes can shadow or expose previously-cached translations.
                self.pmmu_cache_ensure();
                self.pmmu_cache_invalidate();
            }
            Ok(())
        } else if extword & 0b1110_0001_1111_1111 == 0b0100_0000_0000_0000 {
            // PMOVE (format 1)
            self.op_pmove1(instr, extword)
        } else if extword & 0b1110_0001_1111_1111 == 0b0110_0000_0000_0000 {
            // PMOVE (format 2 or 3)
            self.op_pmove3(instr, extword)
        } else if extword & 0b1110_0000_0000_0000 == 0b1000_0000_0000_0000 {
            // PTEST
            self.op_ptest(instr, extword)
        } else {
            // Unknown instruction
            log::warn!(
                "Unimplemented PMMU op 000: {:016b} {:016b}",
                instr.data,
                extword
            );
            self.op_linef(instr)
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

        // Flush disable, inhibits ATC flush on xRP write. 68030+
        let fd = CPU_TYPE >= M68030 && extword.fd();

        match (extword.preg(), extword.write()) {
            (0b000, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.tc.0)?;
            }
            (0b000, false) => {
                let newval = TcReg(self.read_ea(instr, instr.get_op2())?);
                self.regs.pmmu.tc = newval;
                if newval.enable() {
                    if newval.is()
                        + newval.tia() as u32
                        + newval.tib() as u32
                        + newval.tic() as u32
                        + newval.tid() as u32
                        + newval.ps() as u32
                        != 32
                    {
                        bail!("Invalid PMMU configuration: {:?}", newval);
                    }

                    self.pmmu_cache_ensure();
                } else {
                    // Manipulation of TC with E clear causes an ATC flush
                    self.pmmu_cache_invalidate();
                }
            }
            (0b001, true) => {
                self.write_ea_sz::<8>(instr, instr.get_op2(), self.regs.pmmu.drp.0.to_be_bytes())?;
            }
            (0b001, false) => {
                self.regs.pmmu.drp.0 =
                    DoubleLong::from_be_bytes(self.read_ea_sz::<8>(instr, instr.get_op2())?);

                if !fd {
                    self.pmmu_cache_invalidate();
                }
            }
            (0b010, true) => {
                self.write_ea_sz::<8>(instr, instr.get_op2(), self.regs.pmmu.srp.0.to_be_bytes())?;
            }
            (0b010, false) => {
                self.regs.pmmu.srp.0 =
                    DoubleLong::from_be_bytes(self.read_ea_sz::<8>(instr, instr.get_op2())?);

                if !fd {
                    self.pmmu_cache_invalidate();
                }
            }
            (0b011, true) => {
                self.write_ea_sz::<8>(instr, instr.get_op2(), self.regs.pmmu.crp.0.to_be_bytes())?;
            }
            (0b011, false) => {
                self.regs.pmmu.crp.0 =
                    DoubleLong::from_be_bytes(self.read_ea_sz::<8>(instr, instr.get_op2())?);

                if !fd {
                    self.pmmu_cache_invalidate();
                }
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

    pub(in crate::cpu_m68k) fn op_pmove3(
        &mut self,
        instr: &Instruction,
        extword: Word,
    ) -> Result<()> {
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        let extword = Pmove3Extword(extword);

        match (extword.preg(), extword.write()) {
            (0b000, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.psr.0)?;
            }
            (0b000, false) => {
                self.regs.pmmu.psr.0 = self.read_ea(instr, instr.get_op2())?;
            }
            (0b001, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.pcsr.0)?;
            }
            (0b001, false) => {
                self.regs.pmmu.pcsr.0 = self.read_ea(instr, instr.get_op2())?;
            }
            _ => bail!("PMOVE3 invalid Preg: {}", extword.preg()),
        }

        Ok(())
    }

    pub(in crate::cpu_m68k) fn op_pmove_68030(
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
                let newval = TcReg(self.read_ea(instr, instr.get_op2())?);
                self.regs.pmmu.tc = newval;
                if newval.enable() {
                    if newval.is()
                        + newval.tia() as u32
                        + newval.tib() as u32
                        + newval.tic() as u32
                        + newval.tid() as u32
                        + newval.ps() as u32
                        != 32
                    {
                        bail!("Invalid PMMU configuration: {:?}", newval);
                    }

                    self.pmmu_cache_ensure();
                }
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
            _ => bail!("PMOVE 68030 invalid Preg: {}", extword.preg()),
        }

        if !extword.fd() {
            self.pmmu_cache_invalidate();
        }

        Ok(())
    }

    pub(in crate::cpu_m68k) fn op_ptest(
        &mut self,
        instr: &Instruction,
        extword: Word,
    ) -> Result<()> {
        let extword = PtestExtword(extword);

        let fc = self.decode_pmmu_fc(extword.fc(), "PTEST")?;

        let vaddr = self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?;
        let result = self.pmmu_translate_lookup::<true>(fc, vaddr, !extword.read());
        match result {
            Ok(_) => {
                if extword.a_set() {
                    self.regs.write_a(extword.an(), self.regs.pmmu.last_desc);
                }
            }
            // PTEST never raises: PSR reflects what happened.
            Err(_) => (),
        }
        Ok(())
    }

    /// PLOAD - load an ATC entry without raising on failure
    pub(in crate::cpu_m68k) fn op_pload(
        &mut self,
        instr: &Instruction,
        extword: Word,
    ) -> Result<()> {
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        // PLOAD extension word is same as PTEST
        let extword = PtestExtword(extword);

        let fc = self.decode_pmmu_fc(extword.fc(), "PLOAD")?;
        let vaddr = self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?;
        let writing = !extword.read();

        // PLOAD does not affect PSR. The walk inside pmmu_translate may modify PSR
        let saved_psr = self.regs.pmmu.psr;
        // Ignore failures during the walk
        let _ = self.pmmu_translate(fc, vaddr, writing);
        self.regs.pmmu.psr = saved_psr;

        Ok(())
    }

    /// Decodes the 5-bit FC field used by PTEST and PLOAD
    fn decode_pmmu_fc(&self, fc_field: u8, op: &str) -> Result<u8> {
        let fc = if fc_field & 0b10000 != 0 {
            fc_field & 0b1111
        } else if fc_field & 0b11000 == 0b01000 {
            self.regs.read_d(usize::from(fc_field & 0b111))
        } else if fc_field & 0b11111 == 0 {
            self.regs.sfc as u8
        } else if fc_field & 0b11111 == 1 {
            self.regs.dfc as u8
        } else {
            bail!("Invalid FC in {}: {:05b}", op, fc_field);
        };
        Ok(fc & FC_MASK)
    }
}
