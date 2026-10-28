//! M68851 PMMU - Opcode implementations

use crate::cpu_m68k::{FpuM68kType, M68030};
use anyhow::{Result, bail};

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::bus::FC_MASK;
use crate::cpu_m68k::cpu::{CpuError, CpuM68k, ExceptionGroup, VECTOR_PRIVILEGE_VIOLATION};
use crate::cpu_m68k::instruction::Instruction;
use crate::cpu_m68k::pmmu::instruction::{Pmove3Extword, PtestExtword};
use crate::types::{DoubleLong, Long, Word};

use super::instruction::Pmove1Extword;
use super::regs::{RegisterPSR, TcReg, TrTranslationReg};

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
    // The idle cycles of the MMU instructions are given for the 68851 as the
    // 68020 times (m68020) and for the 68030 (m68030). Those of the 68851 are
    // the times of MC68851UM 11.3.2, less 3 cycles for each prefetch and operand
    // bus cycle (the coprocessor interface cycles are not modelled), and those of
    // the 68030 the times of MC68030UM 11.7.2 in the cache case, less the
    // instruction words and the operand bus cycles.

    /// Tells whether the function code field of PFLUSH, PLOAD and PTEST takes the
    /// function code from SFC or DFC, which the 68030 is slower for
    fn pmmu_fc_from_reg(extword: Word) -> bool {
        extword & 0b11110 == 0
    }

    pub(in crate::cpu_m68k) fn op_pop_000(&mut self, instr: &Instruction) -> Result<()> {
        if !PMMU {
            return self.op_linef(instr);
        }

        let extword = self.fetch_immediate::<Word>()?;

        // Mind the ordering here!
        if extword & 0b1111_1101_1110_0000 == 0b0010_0000_0000_0000 {
            // PLOAD
            self.op_pload(instr, extword)
        } else if extword & 0b1110_0010_0000_0000 == 0b0010_0000_0000_0000 {
            // PFLUSH / PFLUSHA / PFLUSHS (M68000PRM 6-32..6-34,
            // MC68851 UM A-6..A-7). Bit 8 is part of the 68851's 4-bit
            // mask and must not gate the dispatch.
            self.op_pflush(instr, extword)
        } else if extword == 0b1010_0000_0000_0000 {
            // PFLUSHR
            // TODO specific flush
            self.read_ea_sz::<8>(instr, instr.get_op2())?;
            self.pmmu_cache_invalidate();
            advance_cycles!(self, m68020: 74)?;
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
                self.advance_cycles(2)?;
            } else {
                // EA to register
                let val = self.read_ea::<Long>(instr, instr.get_op2())?;
                self.regs.pmmu.tt[tt_idx] = TrTranslationReg(val);
                // TT changes can shadow or expose previously-cached translations.
                self.pmmu_cache_ensure();
                self.pmmu_cache_invalidate();
                self.advance_cycles(6)?;
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

        match (extword.preg(), extword.write()) {
            // TC
            (0b000, false) => advance_cycles!(self, m68020: 146)?,
            // DRP, SRP, CRP
            (0b001..=0b011, false) => advance_cycles!(self, m68020: 96)?,
            (0b001..=0b011, true) => advance_cycles!(self, m68020: 72)?,
            // CAL, VAL, SCC, AC
            (_, false) => advance_cycles!(self, m68020: 48)?,
            (_, true) => advance_cycles!(self, m68020: 61)?,
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
        if extword.write() {
            advance_cycles!(self, m68020: 61)?;
        } else {
            advance_cycles!(self, m68020: 48)?;
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

        match (extword.preg(), extword.write()) {
            // TC, with translation enabled or not
            (0b000, false) if self.regs.pmmu.tc.enable() => self.advance_cycles(32)?,
            (0b000, false) => self.advance_cycles(8)?,
            // SRP, CRP
            (_, false) => self.advance_cycles(4)?,
            (_, true) => (),
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
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        let extword = PtestExtword(extword);

        let fc = self.decode_pmmu_fc(extword.fc(), "PTEST")?;
        if extword.a_set() {
            advance_cycles!(self, m68000: 0, m68020: 130, m68030: 18)?;
        } else {
            advance_cycles!(self, m68000: 0, m68020: 104, m68030: 18)?;
        }

        let vaddr = self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?;
        let writing = !extword.read();

        if extword.level() == 0 {
            // M68000PRM 6-62..6-64: level 0 searches the ATC only. With an
            // address-register operand the MC68030 takes an F-line
            // unimplemented instruction exception.
            if extword.a_set() {
                return self.op_linef(instr);
            }

            let mut psr = RegisterPSR::default();
            if self.pmmu_tt_match(fc, vaddr, writing) {
                // M68030UM Table 9-3: T set; the remaining bits are
                // undefined.
                psr.set_transparent(true);
            } else if let Some(entry) = self.pmmu_atc_probe(fc, vaddr) {
                // Snow's ATC entries carry no bus-error bit, so B stays
                // clear.
                if entry.wp {
                    psr.set_write_protected(true);
                }
                if entry.modified {
                    psr.set_modified(true);
                }
            } else {
                psr.set_invalid(true);
            }
            self.regs.pmmu.psr = psr;
            return Ok(());
        }

        // Levels 1-7 search the translation tables to the requested level.
        // PTEST never raises: fault conditions are recorded in the PSR by
        // the walk itself.
        match self.pmmu_translate_lookup::<true>(fc, vaddr, writing, extword.level()) {
            Ok(_) => {}
            Err(e) => match e.downcast_ref::<CpuError>() {
                // Fault-type outcomes and bus errors reading descriptors
                // have already been recorded in the PSR.
                Some(
                    CpuError::Pagefault(_) | CpuError::BusError(_) | CpuError::AddressError(_),
                ) => {}
                other => log::warn!("PTEST unexpected walk error: {other:?}"),
            },
        }
        // M68000PRM 6-63: if the A bit is set, An is loaded with the
        // physical address of the last descriptor successfully fetched
        // during the search, whether or not a valid translation was found.
        if extword.a_set() {
            self.regs.write_a(extword.an(), self.regs.pmmu.last_desc);
        }
        Ok(())
    }

    /// PFLUSH / PFLUSHA (M68000PRM 6-32..6-34) and PFLUSHS (MC68851 UM
    /// A-6..A-7).
    ///
    /// Extension word: bits 15-13 = 001, bits 12-10 = MODE, bit 9 = 0,
    /// MASK, bits 4-0 = FC field. The 68030 mask is 3 bits in 7-5 (bits
    /// 9-8 zero); the 68851 mask is 4 bits in 8-5.
    ///
    /// MODE 001 flushes all entries (PFLUSHA). MODE 100 flushes by function
    /// code. MODE 110 additionally restricts the flush to the page holding
    /// the effective address; the EA is the flush key itself and is never
    /// dereferenced. The 68851 MODEs 101/111 are PFLUSHS, which also flush
    /// globally shared entries.
    pub(in crate::cpu_m68k) fn op_pflush(
        &mut self,
        instr: &Instruction,
        extword: Word,
    ) -> Result<()> {
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        // M68000PRM 6-32: on the 68030 bits 9-8 of the extension word must
        // be zero (the mask is 3 bits in 7-5), so an encoding with bit 8
        // set is not a valid 68030 form and takes the F-line path. On the
        // 68851 bit 8 is part of the 4-bit mask (MC68851 UM A-7).
        if CPU_TYPE >= M68030 && extword & 0b0000_0001_0000_0000 != 0 {
            return self.op_linef(instr);
        }

        let mode = (extword >> 10) & 0b111;
        let mask = if CPU_TYPE < M68030 {
            // MC68851 UM A-7: 4-bit mask in bits 8-5.
            ((extword >> 5) & 0b1111) as u8
        } else {
            // M68000PRM 6-33: 3-bit mask in bits 7-5.
            ((extword >> 5) & 0b111) as u8
        };
        let fc_field = (extword & 0b11111) as u8;

        // The times of the valid forms, the others are F-line exceptions
        let fc_reg = Self::pmmu_fc_from_reg(extword);
        let pflushs = CPU_TYPE < M68030;
        match mode {
            0b001 => advance_cycles!(self, m68000: 0, m68020: 34, m68030: 8)?,
            0b100 => {
                advance_cycles!(self, m68000: 0, m68020: 70, m68030: if fc_reg { 16 } else { 12 })?;
            }
            0b101 if pflushs => advance_cycles!(self, m68000: 0, m68020: 70, m68030: 0)?,
            0b110 => {
                advance_cycles!(self, m68000: 0, m68020: 102, m68030: if fc_reg { 16 } else { 12 })?;
            }
            0b111 if pflushs => advance_cycles!(self, m68000: 0, m68020: 102, m68030: 0)?,
            _ => (),
        }

        match mode {
            // PFLUSHA: invalidate all entries (mask and FC field are zero).
            0b001 => self.pmmu_cache_invalidate(),
            // PFLUSH fc,#mask: flush by function code. Snow's ATC slot for a
            // page is shared across function codes and there is no per-FC
            // index, so honouring this precisely would mean scanning every
            // slot (up to 2^20). The hardware may drop ATC entries at any
            // time, so a full flush is always legal; do the cheap O(1)
            // generation bump instead.
            0b100 => self.pmmu_cache_invalidate(),
            // PFLUSHS fc,#mask (68851 only): also flushes globally shared
            // entries. Snow's ATC does not track the SG bit, so a full
            // flush is the only correct over-flush. The 68030 does not
            // implement PFLUSHS and takes an F-line emulator exception
            // instead (M68030UM 9.8).
            0b101 | 0b111 if CPU_TYPE < M68030 => self.pmmu_cache_invalidate(),
            // PFLUSH fc,#mask,<ea>: flush by function code and page.
            0b110 => {
                let fc = self.decode_pmmu_fc(fc_field, "PFLUSH")?;
                let vaddr =
                    self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                self.pmmu_flush(fc, mask, vaddr);
            }
            _ => return self.op_linef(instr),
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
        let fc_reg = Self::pmmu_fc_from_reg(extword.0);
        advance_cycles!(
            self,
            m68000: 0,
            m68020: 94,
            m68030: if fc_reg { 8 } else { 4 },
        )?;
        let vaddr = self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?;
        let writing = !extword.read();

        // M68030UM 9.8: any existing entry translating this address is
        // flushed before the search, so a failed walk leaves the address
        // untranslated rather than served by a stale entry. Mask 0 matches
        // every function code the shared slot has served.
        self.pmmu_flush(0, 0, vaddr);

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
