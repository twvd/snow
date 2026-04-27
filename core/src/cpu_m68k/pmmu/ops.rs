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
use super::regs::TcReg;

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

        if extword & 0b1110_0011_0000_0000 == 0b0010_0000_0000_0000 {
            // PFLUSHx family
            if extword == 0b0010_0100_0000_0000 {
                // PFLUSHA - invalidate full cache
                self.pmmu_cache_invalidate();
            } else {
                bail!("Unimplemented PMMU op: PFLUSH extword={:016b}", extword);
            }
            Ok(())
        } else if extword == 0b1010_0000_0000_0000 {
            // PFLUSHR
            bail!("Unimplemented PMMU op: PFLUSHR extword={:016b}", extword);
        } else if extword & 0b1111_1101_1110_0000 == 0b0010_0000_0000_0000 {
            // PLOAD
            bail!("Unimplemented PMMU op: PLOAD extword={:016b}", extword);
        } else if extword & 0b1110_0001_1111_1111 == 0b0100_0000_0000_0000 {
            // PMOVE (format 1)
            self.op_pmove1(instr, extword)
        } else if extword & 0b1110_0001_1111_1111 == 0b0110_0000_0000_0000 {
            // PMOVE (format 2 or 3)
            self.op_pmove3(instr, extword)
        } else if extword & 0b1110_0000_0000_0000 == 0b1000_0000_0000_0000 {
            // PTEST
            self.op_ptest(instr, extword)
        } else if extword & 0b1110_0000_1111_1111 == 0b0100_0000_0000_0000 && CPU_TYPE >= M68030 {
            // PMOVE 68030 version
            self.op_pmove_68030(instr, extword)
        } else if extword & 0b1111_1000_1111_1111 == 0b0000_1000_0000_0000 && CPU_TYPE >= M68030 {
            // PMOVE TT regs (68030+)
            let tt_idx = ((extword >> 10) & 1) as usize;
            let reg_to_ea = extword & (1 << 9) != 0;
            if reg_to_ea {
                let v = self.regs.pmmu.tt[tt_idx].0;
                self.write_ea::<Long>(instr, instr.get_op2(), v)?;
            } else {
                let v: Long = self.read_ea::<Long>(instr, instr.get_op2())?;
                self.regs.pmmu.tt[tt_idx].0 = v;
            }
            Ok(())
        } else {
            // Unknown instruction
            bail!(
                "Unimplemented PMMU op 000: instr={:016b} extword={:016b}",
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

        // FD = "function disabled" (suppress automatic ATC flush).
        if extword.fd() {
            bail!(
                "Unimplemented PMMU bit: PMOVE format 1 FD bit set (preg={:03b})",
                extword.preg()
            );
        }

        match (extword.preg(), extword.write()) {
            (0b000, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.tc.0)?;
            }
            (0b000, false) => {
                let newval = TcReg(self.read_ea(instr, instr.get_op2())?);
                if newval.fcl() {
                    bail!("Unimplemented PMMU bit: TC.FCL (Function Code Lookup) set");
                }
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
                    self.pmmu_cache_invalidate();
                }
            }
            (0b001, true) => {
                bail!("Unimplemented PMMU op: read of DRP register");
            }
            (0b001, false) => {
                bail!("Unimplemented PMMU op: write to DRP register");
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
                let v: u8 = self.read_ea(instr, instr.get_op2())?;
                if v != 0 {
                    bail!(
                        "Unimplemented PMMU op: write to CAL (Current Access Level) register, value={:02X}",
                        v
                    );
                }
                self.regs.pmmu.cal.0 = v;
            }
            (0b101, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.val.0)?;
            }
            (0b101, false) => {
                let v: u8 = self.read_ea(instr, instr.get_op2())?;
                if v != 0 {
                    bail!(
                        "Unimplemented PMMU op: write to VAL (Validate Access Level) register, value={:02X}",
                        v
                    );
                }
                self.regs.pmmu.val.0 = v;
            }
            (0b110, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.scc)?;
            }
            (0b110, false) => {
                let v: u8 = self.read_ea(instr, instr.get_op2())?;
                if v != 0 {
                    bail!(
                        "Unimplemented PMMU op: write to SCC (Stack Change Control) register, value={:02X}",
                        v
                    );
                }
                self.regs.pmmu.scc = v;
            }
            (0b111, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.ac.0)?;
            }
            (0b111, false) => {
                let v: u16 = self.read_ea(instr, instr.get_op2())?;
                if v != 0 {
                    bail!(
                        "Unimplemented PMMU op: write to AC (Access Control) register, value={:04X}",
                        v
                    );
                }
                self.regs.pmmu.ac.0 = v;
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
                let v: u16 = self.read_ea(instr, instr.get_op2())?;
                if v != 0 {
                    bail!(
                        "Unimplemented PMMU op: write to PSR (status register), value={:016b}",
                        v
                    );
                }
                self.regs.pmmu.psr.0 = v;
            }
            (0b001, true) => {
                self.write_ea(instr, instr.get_op2(), self.regs.pmmu.pcsr.0)?;
            }
            (0b001, false) => {
                let v: u16 = self.read_ea(instr, instr.get_op2())?;
                if v != 0 {
                    bail!(
                        "Unimplemented PMMU op: write to PCSR (cache status register), value={:016b}",
                        v
                    );
                }
                self.regs.pmmu.pcsr.0 = v;
            }
            _ => bail!(
                "Unimplemented PMMU op: PMOVE3 invalid/reserved Preg: {:03b}",
                extword.preg()
            ),
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
                if newval.fcl() {
                    bail!("Unimplemented PMMU bit: TC.FCL (Function Code Lookup) set");
                }
                self.regs.pmmu.tc = newval;
                if newval.enable()
                    && newval.is()
                        + newval.tia() as u32
                        + newval.tib() as u32
                        + newval.tic() as u32
                        + newval.tid() as u32
                        + newval.ps() as u32
                        != 32
                {
                    bail!("Invalid PMMU configuration: {:?}", newval);
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
            _ => bail!(
                "Unimplemented PMMU op: PMOVE 68030 unsupported Preg: {:03b} (write={})",
                extword.preg(),
                extword.write()
            ),
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

        let fc = if extword.fc() & 0b10000 != 0 {
            extword.fc() & 0b1111
        } else if extword.fc() & 0b11000 == 0b01000 {
            self.regs.read_d(usize::from(extword.fc() & 0b111))
        } else if extword.fc() & 0b11111 == 0 {
            self.regs.sfc as u8
        } else if extword.fc() & 0b11111 == 1 {
            self.regs.dfc as u8
        } else {
            bail!("Invalid FC in PTEST: {:05b}", extword.fc());
        } & FC_MASK;

        let vaddr = self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?;
        let result =
            self.pmmu_translate_lookup::<true>(fc, vaddr, !extword.read(), extword.level());
        match result {
            Ok(_) => {
                if extword.a_set() {
                    self.regs.write_a(extword.an(), self.regs.pmmu.last_desc);
                }
            }
            // PTEST never raises: PSR reflects what happened.
            // This may swallow a bus error
            Err(_) => (),
        }
        Ok(())
    }
}
