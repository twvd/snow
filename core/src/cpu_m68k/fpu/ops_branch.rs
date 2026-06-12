use crate::cpu_m68k::FpuM68kType;
use anyhow::{Result, bail};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::cpu::{CpuM68k, ExceptionGroup, VECTOR_TRAPV};
use crate::cpu_m68k::instruction::Instruction;
use crate::types::{Byte, Word};

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
    /// Condition test for FBcc/FDBcc
    fn fcc(&self, cc: usize) -> Result<(bool, bool)> {
        let nan = self.regs.fpu.fpsr.fpcc_nan();
        let zero = self.regs.fpu.fpsr.fpcc_z();
        let neg = self.regs.fpu.fpsr.fpcc_n();

        Ok(match cc & 0b111111 {
            // IEEE Aware Tests (never set BSUN)
            0b000001 => (zero, false),                   // EQ: Equal
            0b001110 => (!zero, false),                  // NE: Not Equal
            0b000010 => (!nan && !zero && !neg, false),  // OGT: Ordered Greater Than
            0b001101 => (nan || zero || neg, false),     // ULE: Unordered or Less or Equal
            0b000011 => (zero || (!nan && !neg), false), // OGE: Ordered Greater Than or Equal
            0b001100 => (nan || (neg && !zero), false),  // ULT: Unordered or Less Than
            0b000100 => (neg && !nan && !zero, false),   // OLT: Ordered Less Than
            0b001011 => (nan || zero || !neg, false),    // UGE: Unordered or Greater or Equal
            0b000101 => (zero || (neg && !nan), false),  // OLE: Ordered Less Than or Equal
            0b001010 => (nan || (!neg && !zero), false), // UGT: Unordered or Greater Than
            0b000110 => (!nan && !zero, false),          // OGL: Ordered Greater or Less Than
            0b001001 => (nan || zero, false),            // UEQ: Unordered or Equal
            0b000111 => (!nan, false),                   // OR: Ordered
            0b001000 => (nan, false),                    // UN: Unordered

            // IEEE Nonaware Tests (set BSUN for all except EQ and NE)
            0b010010 => (!nan && !zero && !neg, nan), // GT: Greater Than
            0b011101 => (nan || zero || neg, nan),    // NGT: Not Greater Than
            0b010011 => (zero || (!nan && !neg), nan), // GE: Greater Than or Equal
            0b011100 => (nan || (neg && !zero), nan), // NGE: Not (Greater Than or Equal)
            0b010100 => (neg && !nan && !zero, nan),  // LT: Less Than
            0b011011 => (nan || zero || !neg, nan),   // NLT: Not Less Than
            0b010101 => (zero || (neg && !nan), nan), // LE: Less Than or Equal
            0b011010 => (nan || (!neg && !zero), nan), // NLE: Not (Less Than or Equal)
            0b010110 => (!nan && !zero, nan),         // GL: Greater or Less Than
            0b011001 => (nan || zero, nan),           // NGL: Not (Greater or Less Than)
            0b010111 => (!nan, nan),                  // GLE: Greater, Less or Equal
            0b011000 => (nan, nan),                   // NGLE: Not (Greater, Less or Equal)

            // Miscellaneous Tests
            0b000000 => (false, false), // F: False
            0b001111 => (true, false),  // T: True
            0b010000 => (false, nan),   // SF: Signaling False
            0b011111 => (true, nan),    // ST: Signaling True
            0b010001 => (zero, nan),    // SEQ: Signaling Equal
            0b011110 => (!zero, nan),   // SNE: Signaling Not Equal

            _ => bail!("Unknown Fcc predicate"),
        })
    }

    /// FBcc
    pub(in crate::cpu_m68k) fn op_fbcc<const L: bool>(
        &mut self,
        instr: &Instruction,
    ) -> Result<()> {
        let displacement = if L {
            let msb = self.fetch_pump()? as Address;
            let lsb = self.fetch_pump()? as Address;
            // -4 since we just nudged the PC twice
            ((msb << 16) | lsb) as i32 - 4
        } else {
            let lsb = self.fetch_pump()?;
            lsb as i16 as i32 - 2
        };

        self.advance_cycles(2)?; // idle

        let (test, bsun) = self.fcc(instr.get_fcc())?;
        self.regs.fpu.fpsr.exs_mut().set_bsun(bsun);
        if test {
            // Branch taken
            self.history_current.branch_taken = Some(true);

            let pc = self
                .regs
                .pc
                .wrapping_add_signed(displacement)
                .wrapping_add(2);
            self.set_pc(pc)?;
        } else {
            // Branch not taken
            self.history_current.branch_taken = Some(false);

            self.advance_cycles(2)?; // idle
        }
        Ok(())
    }

    /// FScc.b
    pub(in crate::cpu_m68k) fn op_fscc(&mut self, instr: &Instruction) -> Result<()> {
        let cc = usize::from(self.fetch()? & 0b111111);
        let (test, bsun) = self.fcc(cc)?;
        self.regs.fpu.fpsr.exs_mut().set_bsun(bsun);

        self.write_ea::<Byte>(instr, instr.get_op2(), if test { 0xFF } else { 0 })?;
        Ok(())
    }

    /// FTRAPcc (68020+) - OPERAND_WORDS is 0, 1 (word) or 2 (long)
    pub(in crate::cpu_m68k) fn op_ftrapcc<const OPERAND_WORDS: usize>(
        &mut self,
        _instr: &Instruction,
    ) -> Result<()> {
        let cc = usize::from(self.fetch()? & 0b111111);
        let (test, bsun) = self.fcc(cc)?;
        self.regs.fpu.fpsr.exs_mut().set_bsun(bsun);

        // Consume the immediate operand from the prefetch queue and refill.
        // After this, regs.pc points to the instruction following FTRAPcc, which
        // is also the address the exception stack frame must capture.
        for _ in 0..OPERAND_WORDS {
            let _: Word = self.fetch_pump()?;
        }
        self.prefetch_pump()?;

        if !test {
            return Ok(());
        }

        self.raise_exception(ExceptionGroup::Group2, VECTOR_TRAPV, None)
    }

    /// FDBcc
    pub(in crate::cpu_m68k) fn op_fdbcc(&mut self, instr: &Instruction) -> Result<()> {
        let cc = usize::from(self.fetch()? & 0b111111);
        let displacement = self.fetch_pump()? as i16 as i32 - 2;

        self.advance_cycles(2)?; // idle

        let (test, bsun) = self.fcc(cc)?;
        self.regs.fpu.fpsr.exs_mut().set_bsun(bsun);

        if !test {
            let dn = self.regs.read_d::<Word>(instr.get_op2()).wrapping_sub(1);
            self.regs.write_d::<Word>(instr.get_op2(), dn);

            if dn != 0xFFFF {
                self.history_current.branch_taken = Some(true);

                let pc = self
                    .regs
                    .pc
                    .wrapping_add_signed(displacement)
                    .wrapping_add(2);
                self.set_pc(pc)?;

                // Trigger address error now if unaligned..
                self.prefetch_refill()?;
            } else {
                // Loop terminated
                self.history_current.branch_taken = Some(false);
                self.advance_cycles(4)?; // idle
            }
        } else {
            self.history_current.branch_taken = Some(false);
            self.advance_cycles(2)?; // idle
        }

        Ok(())
    }
}
