use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::cache::CpuCaches;
use anyhow::{Result, bail};
use arpfloat::Float;
use either::Either;
use num_traits::ToPrimitive;
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::{
    CpuError, CpuM68k, ExceptionGroup, PrefetchWord, VECTOR_FORMAT_ERROR, VECTOR_LINEF,
    VECTOR_PRIVILEGE_VIOLATION,
};
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::arith::Range;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord, opmode};
use crate::cpu_m68k::fpu::regs::{FpuRegisterFile, Unimp040};
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::cpu_m68k::fpu::timings::FpuOperand;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
use crate::cpu_m68k::{CpuM68kType, CpuSized, FPU_M68040, FPU_M68881, FPU_M68882};
use crate::tickable::Ticks;
use crate::types::{Byte, Long, Word};

use super::storage::{DOUBLE_SIZE, EXTENDED_SIZE, PACKED_SIZE, SINGLE_SIZE};

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
    TCaches,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU, TCaches>
where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    /// Takes the unimplemented floating-point instruction exception of the
    /// MC68040 for the instruction of `extword`, once its source operand is
    /// fetched (MC68040UM 9.6.1). The FPU keeps the instruction and its
    /// operands, `etemp` and `fptemp` with their tags, for FSAVE; `packed` for
    /// a packed decimal real source, which ETEMP and FPTEMP hold as it is.
    fn fpu_unimp040(
        &mut self,
        extword: FmoveExtWord,
        (etemp, stag): ([Long; 3], u8),
        (fptemp, dtag): ([Long; 3], u8),
        packed: bool,
    ) -> Result<()> {
        self.regs.fpu.unimp040 = Some(Unimp040 {
            cmdreg1b: extword.0,
            etemp,
            stag,
            fptemp,
            dtag,
            packed,
        });
        // With the prefetch queue refilled, the PC is that of the next
        // instruction, which the exception stack frame holds
        self.prefetch_pump()?;
        self.step_fpu_unimp = true;
        self.raise_exception(ExceptionGroup::Group2, VECTOR_LINEF, None)
    }

    /// Spends the time of an arithmetic operation of `opmode` on an `operand`,
    /// less `less`. The CPU only waits for the transfer of the operation, and the
    /// FPU runs the rest on its own, until the next FPU instruction
    /// (MC68881/MC68882UM 5.1.1.1, 8.6).
    ///
    /// On the 68882, an FMOVE to FPn in single, double or extended format has no
    /// tail, or one of 6 clocks when it runs with less concurrency.
    ///
    /// The 68882 can also start the next arithmetic operation before the end of this
    /// one: see fpu_head().
    fn fpu_arith_cycles(
        &mut self,
        extword: FmoveExtWord,
        operand: FpuOperand,
        less: Ticks,
    ) -> Result<()> {
        let timings = Self::fpu_timings();
        let total = timings.alu(extword.opmode(), operand).saturating_sub(less);
        let cpu = if FPU_TYPE == FPU_M68882
            && extword.opmode() == 0
            && matches!(
                operand,
                FpuOperand::Single | FpuOperand::Double | FpuOperand::Extended
            ) {
            if self.fpu_fmove_degraded(1 << extword.dst_reg()) {
                total - 6
            } else {
                total
            }
        } else {
            match timings.alu_cpu(operand) {
                Some(cpu) => cpu.saturating_sub(less).min(total),
                None => total,
            }
        };
        self.advance_cycles(cpu)?;
        self.fpu_free_at = self.cycles + total - cpu;
        if total > cpu {
            self.fpu_busy_regs = Self::fpu_dest_regs(extword);
        }
        Ok(())
    }

    /// Returns the FPU data registers that the arithmetic operation of `extword`
    /// writes, a bit for each
    fn fpu_dest_regs(extword: FmoveExtWord) -> u8 {
        let dst = 1 << extword.dst_reg();
        match extword.opmode() {
            // FCMP, FTST
            0b0111000 | 0b0111010 => 0,
            // FSINCOS
            0b0110000..=0b0110111 => dst | 1 << (extword.opmode() & 0b111),
            _ => dst,
        }
    }

    /// Tells whether an FMOVE to FPn on the 68882 that uses the FPU data registers
    /// `regs` runs with less concurrency: when the operation the FPU still runs
    /// writes one of them, or when the rounding precision is single or double
    /// (MC68881/MC68882UM 5.1.2.2, Table 5-5). Operands that are NANs, unnormalized
    /// or denormalized do the same, which is not modelled.
    fn fpu_fmove_degraded(&self, regs: u8) -> bool {
        self.fpu_busy_regs & regs != 0 || self.regs.fpu.fpcr.prec() != 0
    }

    /// Returns the head of the arithmetic operation that the general FPU instruction
    /// `opcode` starts, from its extension word at the front of the prefetch queue:
    /// the time at its start that the 68882 overlaps with the end of the previous
    /// operation (MC68881/MC68882UM 8.5.1.3, H column of Table 8-3, about the same
    /// for every operation in a format but FMOVE). Returns 0 on the 68881, which
    /// cannot overlap two operations, and for the other instructions.
    pub(in crate::cpu_m68k) fn fpu_head(&self, opcode: Word) -> Ticks {
        if opcode & 0b0000_0001_1100_0000 != 0 {
            return 0;
        }
        let extword = match self.prefetch.front() {
            Some(PrefetchWord::Word(w)) => FmoveExtWord(*w),
            _ => return 0,
        };
        let fmove = FPU_TYPE == FPU_M68882 && extword.opmode() == 0;
        let operand = match (extword.subop(), extword.src_spec()) {
            // FMOVE FPm,FPn on the 68882
            (0b000, _) if fmove => {
                let regs = 1 << extword.src_spec() | 1 << extword.dst_reg();
                return if self.fpu_fmove_degraded(regs) {
                    10
                } else {
                    21
                };
            }
            (0b000, _) => FpuOperand::Fpn,
            // FMOVE <ea>,FPn in single, double or extended format on the 68882
            (0b010, spec @ (0b001 | 0b101 | 0b010)) if fmove => {
                let head = match spec {
                    0b001 => 34,
                    0b101 => 40,
                    _ => 46,
                };
                return if self.fpu_fmove_degraded(1 << extword.dst_reg()) {
                    head - 6
                } else {
                    head
                };
            }
            (0b010, 0b000 | 0b100 | 0b110) => FpuOperand::Int,
            (0b010, 0b001) => FpuOperand::Single,
            (0b010, 0b101) => FpuOperand::Double,
            (0b010, 0b010) => FpuOperand::Extended,
            (0b010, 0b011) => FpuOperand::Packed,
            // FMOVE FPm,<ea> in single, double or extended format on the 68882,
            // which waits for the end of the previous operation if that writes FPm
            (0b011, fmt @ (0b001 | 0b101 | 0b010)) if FPU_TYPE == FPU_M68882 => {
                return if self.fpu_busy_regs & 1 << extword.src_reg() != 0 {
                    0
                } else {
                    match fmt {
                        0b001 => 38,
                        0b101 => 44,
                        _ => 50,
                    }
                };
            }
            _ => return 0,
        };
        Self::fpu_timings().alu_head(operand)
    }

    /// FNOP
    pub(in crate::cpu_m68k) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        self.regs.fpu.used = true;

        // Fetch second word (0000)
        self.fetch()?;

        self.advance_cycles(Self::fpu_timings().nop())?;

        Ok(())
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn op_fsave(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }
        if !self.regs.fpu.used {
            // Null state: nothing since a reset or the restore of a null frame
            // (MC68881UM 6.4.2.1)
            self.write_ea_sz(instr, instr.get_op2(), [0u8; 4])?;
            self.advance_cycles(Self::fpu_timings().save())?;
            return Ok(());
        }
        // Idle state frame
        match FPU_TYPE {
            FPU_M68881 => {
                let mut stateframe = [0u8; 28];
                stateframe[0..4].copy_from_slice(&0x1F180000u32.to_be_bytes());
                self.write_ea_sz(instr, instr.get_op2(), stateframe)?;
            }
            FPU_M68882 => {
                let mut stateframe = [0u8; 60];
                stateframe[0..4].copy_from_slice(&0x1F380000u32.to_be_bytes());
                self.write_ea_sz(instr, instr.get_op2(), stateframe)?;
            }
            // FSAVE takes a pending exception out of the FPU, so that the other
            // floating-point instructions of the handler don't report it
            // again (MC68040UM 9.8)
            FPU_M68040 => match self.regs.fpu.unimp040.take() {
                Some(unimp) => self.write_ea_sz(instr, instr.get_op2(), unimp.to_frame())?,
                None => self.write_ea_sz(instr, instr.get_op2(), 0x41000000u32.to_be_bytes())?,
            },
            _ => todo!(),
        };

        self.advance_cycles(Self::fpu_timings().save())?;

        Ok(())
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn op_frestore(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            self.advance_cycles(4)?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }
        let state = self.read_ea::<Long>(instr, instr.get_op2())?;
        if state & 0xFF000000 == 0 {
            // NULL state frame, reset FPU
            self.regs.fpu = FpuRegisterFile::default();
        } else {
            // Idle state frame
            self.regs.fpu.used = true;
            // We've already read 4 bytes
            self.step_ea_addr = None;
            match FPU_TYPE {
                FPU_M68881 if state & 0xFF000000 == 0x1F000000 => {
                    self.read_ea_sz::<{ 28 - 4 }>(instr, instr.get_op2())?;
                }
                FPU_M68882 if state & 0xFF000000 == 0x1F000000 => {
                    self.read_ea_sz::<{ 60 - 4 }>(instr, instr.get_op2())?;
                }
                // The MC68040 idle frame is only the format long word (9-41)
                FPU_M68040 if state & 0xFFFF0000 == 0x41000000 => {
                    self.regs.fpu.unimp040 = None;
                }
                FPU_M68040 if state & 0xFFFF0000 == Unimp040::FRAME_FORMAT => {
                    let rest =
                        self.read_ea_sz::<{ Unimp040::FRAME_SIZE - 4 }>(instr, instr.get_op2())?;
                    let mut frame = [0; Unimp040::FRAME_SIZE];
                    frame[..4].copy_from_slice(&state.to_be_bytes());
                    frame[4..].copy_from_slice(&rest);
                    self.regs.fpu.unimp040 = Some(Unimp040::from_frame(&frame));
                    log::warn!(
                        "FRESTORE of an MC68040 unimplemented instruction state frame: \
                         its exception is not reported"
                    );
                }
                // Busy frame, 100 bytes
                FPU_M68040 if state & 0xFFFF0000 == 0x41600000 => {
                    self.read_ea_sz::<{ 100 - 4 }>(instr, instr.get_op2())?;
                    self.regs.fpu.unimp040 = None;
                    log::warn!(
                        "FRESTORE of an MC68040 busy state frame: its exception is not reported"
                    );
                }
                _ => {
                    // Not a frame of this FPU: format error, at the FRESTORE
                    // with the frame left alone (M68030UM table 8-6)
                    log::warn!("Unknown FPU state frame restored: {:08X}", state);
                    if let Some(regs) = self.restart_regs.take() {
                        regs.restore(&mut self.regs);
                    }
                    return self.raise_exception(ExceptionGroup::Group2, VECTOR_FORMAT_ERROR, None);
                }
            };
        }

        let timings = Self::fpu_timings();
        self.advance_cycles(if state & 0xFF000000 == 0 {
            timings.restore_null()
        } else {
            timings.restore()
        })?;

        Ok(())
    }

    /// FMOVE, ALU operations
    pub(in crate::cpu_m68k) fn op_f000(&mut self, instr: &Instruction) -> Result<()> {
        self.regs.fpu.used = true;

        // Fetch extension word
        let extword = FmoveExtWord(self.fetch_pump()?);

        // All but the moves of control registers and FMOVEM can raise an
        // exception, so they leave their address here (MC68881UM FPIAR)
        if matches!(extword.subop(), 0b000 | 0b010 | 0b011) {
            self.regs.fpu.fpiar = self.step_start_pc;
        }

        match extword.subop() {
            0b000 => {
                // Data reg to data reg, with ALU
                let src = self.regs.fpu.fp[extword.src_spec() as usize];
                let dest = self.regs.fpu.fp[extword.dst_reg()];
                let opmode = extword.opmode();

                if FPU_TYPE == FPU_M68040 && !opmode::implemented_040(opmode) {
                    return self.fpu_unimp040(
                        extword,
                        Unimp040::operand(&src),
                        Unimp040::operand(&dest),
                        false,
                    );
                }
                self.regs.fpu.fp[extword.dst_reg()] = self.fpu_alu_op(opmode, &src, &dest)?;
                self.fpu_arith_cycles(extword, FpuOperand::Fpn, 0)?;
            }
            0b100 | 0b101 => self.op_fmovem_control(instr, extword)?,
            0b010 => {
                // EA to FPU register, with ALU op
                let fpx = extword.dst_reg();
                let unimp040 = FPU_TYPE == FPU_M68040 && !opmode::implemented_040(extword.opmode());
                if unimp040 && extword.src_spec() == 0b011 {
                    // A packed decimal real source stays as it is, bits 63-0
                    // in ETEMP and bits 95-64 in FPTEMP (MC68040UM table 9-16)
                    let packed: [Long; 3] = if instr.get_addr_mode()? == AddressingMode::Immediate {
                        [
                            self.fetch_immediate()?,
                            self.fetch_immediate()?,
                            self.fetch_immediate()?,
                        ]
                    } else {
                        let ea = self.calc_ea_addr_sz::<PACKED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        [
                            self.read_ticks(ea)?,
                            self.read_ticks(ea.wrapping_add(4))?,
                            self.read_ticks(ea.wrapping_add(8))?,
                        ]
                    };
                    let dtag = Unimp040::operand(&self.regs.fpu.fp[fpx]).1;
                    return self.fpu_unimp040(
                        extword,
                        ([0, packed[1], packed[2]], 0),
                        ([0, 0, packed[0]], dtag),
                        true,
                    );
                }
                let mut inex1 = false;
                let (value_in, operand) = match extword.src_spec() {
                    0b000 => {
                        // Long
                        (
                            BitsExtReal::from(&Float::from_i64(
                                SEMANTICS_EXTENDED,
                                self.read_ea::<Long>(instr, instr.get_op2())? as i32 as i64,
                            )),
                            FpuOperand::Int,
                        )
                    }
                    0b110 => {
                        // Byte
                        (
                            BitsExtReal::from(&Float::from_i64(
                                SEMANTICS_EXTENDED,
                                self.read_ea::<Byte>(instr, instr.get_op2())? as i8 as i64,
                            )),
                            FpuOperand::Int,
                        )
                    }
                    0b100 => {
                        // Word
                        (
                            BitsExtReal::from(&Float::from_i64(
                                SEMANTICS_EXTENDED,
                                self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i64,
                            )),
                            FpuOperand::Int,
                        )
                    }
                    0b101 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Double-precision real (immediate)
                        (self.read_fpu_double_imm()?, FpuOperand::Double)
                    }
                    0b101 => {
                        // Double-precision real
                        let ea = self.calc_ea_addr_sz::<DOUBLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_double(ea)?, FpuOperand::Double)
                    }
                    0b001 if instr.get_addr_mode()? == AddressingMode::DataRegister => {
                        // Single-precision real (Dn)
                        (
                            self.read_fpu_single_dn(instr.get_op2())?,
                            FpuOperand::Single,
                        )
                    }
                    0b001 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Single-precision real (immediate)
                        (self.read_fpu_single_imm()?, FpuOperand::Single)
                    }
                    0b001 => {
                        // Single-precision real
                        let ea = self.calc_ea_addr_sz::<SINGLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_single(ea)?, FpuOperand::Single)
                    }
                    0b010 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Extended-precision real (immediate)
                        (self.read_fpu_extended_imm()?, FpuOperand::Extended)
                    }
                    0b010 => {
                        // Extended-precision real
                        let ea = self.calc_ea_addr_sz::<EXTENDED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_extended(ea)?, FpuOperand::Extended)
                    }
                    0b111 if FPU_TYPE == FPU_M68040 => {
                        // FMOVECR is not in the hardware of the MC68040
                        // (MC68040UM table 9-10)
                        return self.fpu_unimp040(
                            extword,
                            Unimp040::operand(&BitsExtReal::zero(false)),
                            Unimp040::operand(&self.regs.fpu.fp[fpx]),
                            false,
                        );
                    }
                    0b111 => {
                        // Rounded to the precision of FPCR, which sets INEX2 as for any
                        // result, and the rest of the exception byte is cleared
                        // (MC68881UM FMOVECR)
                        self.regs.fpu.fpsr.exs_mut().0 = 0;
                        let value = self.fpu_rom_constant(extword.movecr_offset() as u8);
                        self.regs.fpu.fp[fpx] = value;
                        self.fpu_condition_codes(&value);
                        let excs = self.regs.fpu.fpsr;
                        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
                        self.advance_cycles(Self::fpu_timings().movecr())?;
                        return Ok(());
                    }
                    0b011 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // BCD packed decimal real (immediate)
                        let (value, inexact) = self.read_fpu_packed_imm()?;
                        inex1 = inexact;
                        (value, FpuOperand::Packed)
                    }
                    0b011 => {
                        // BCD packed decimal real
                        let ea = self.calc_ea_addr_sz::<PACKED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        let (value, inexact) = self.read_fpu_packed(ea)?;
                        inex1 = inexact;
                        (value, FpuOperand::Packed)
                    }
                    _ => {
                        bail!(CpuError::IllegalInstruction);
                    }
                };

                let dest = self.regs.fpu.fp[fpx];
                if unimp040 {
                    return self.fpu_unimp040(
                        extword,
                        Unimp040::operand(&value_in),
                        Unimp040::operand(&dest),
                        false,
                    );
                }
                let result = self.fpu_alu_op(extword.opmode(), &value_in, &dest)?;
                // FTST and FCMP leave the destination as it is, whatever they compare
                self.regs.fpu.fp[fpx] = result;
                if inex1 {
                    // An inexact conversion of the source (MC68881UM 6.1.8)
                    self.regs.fpu.fpsr.exs_mut().set_inex1(true);
                    let excs = self.regs.fpu.fpsr;
                    self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
                }

                let less = if instr.get_addr_mode()? == AddressingMode::DataRegister {
                    Self::fpu_timings().dn_src()
                } else {
                    0
                };
                self.fpu_arith_cycles(extword, operand, less)?;
            }
            0b011 => self.op_fmove_out(instr, extword)?,
            0b110 | 0b111 => self.op_fmovem(instr, extword)?,
            _ => {
                bail!(CpuError::IllegalInstruction);
            }
        }

        Ok(())
    }

    /// FMOVE FPn to memory or a data register, rounded to the size of the destination
    /// format (MC68881UM FMOVE). The condition codes are not affected.
    fn op_fmove_out(&mut self, instr: &Instruction, extword: FmoveExtWord) -> Result<()> {
        let fpx = extword.src_reg();
        let in_dn = instr.get_addr_mode()? == AddressingMode::DataRegister;
        let timings = Self::fpu_timings();
        let less = if in_dn { timings.dn_dst() } else { 0 };
        let cycles = timings.store(extword.dest_fmt()).saturating_sub(less);
        self.regs.fpu.fpsr.exs_mut().0 = 0;

        match extword.dest_fmt() {
            0b000 => {
                let out = self.fpu_to_integer(fpx, i32::MIN.into(), i32::MAX.into(), 32);
                self.fpu_store(instr, in_dn, cycles, out as Long)?;
            }
            0b100 => {
                let out = self.fpu_to_integer(fpx, i16::MIN.into(), i16::MAX.into(), 16);
                self.fpu_store(instr, in_dn, cycles, out as Word)?;
            }
            0b110 => {
                let out = self.fpu_to_integer(fpx, i8::MIN.into(), i8::MAX.into(), 8);
                self.fpu_store(instr, in_dn, cycles, out as Byte)?;
            }
            0b001 => {
                let out = self.fpu_to_real(fpx, Range::SINGLE).to_single();
                self.fpu_store(instr, in_dn, cycles, out)?;
            }
            0b101 if !in_dn => {
                let ea = self.calc_ea_addr_sz::<DOUBLE_SIZE>(
                    instr,
                    instr.get_addr_mode()?,
                    instr.get_op2(),
                )?;
                let out = self.fpu_to_real(fpx, Range::DOUBLE);
                self.advance_cycles(cycles)?;
                self.write_fpu_double(ea, out)?;
            }
            0b010 if !in_dn => {
                let ea = self.calc_ea_addr_sz::<EXTENDED_SIZE>(
                    instr,
                    instr.get_addr_mode()?,
                    instr.get_op2(),
                )?;
                let out = self.fpu_to_real(fpx, Range::EXTENDED);
                self.advance_cycles(cycles)?;
                self.write_fpu_extended(ea, out)?;
            }
            0b011 | 0b111 if !in_dn => {
                let ea = self.calc_ea_addr_sz::<PACKED_SIZE>(
                    instr,
                    instr.get_addr_mode()?,
                    instr.get_op2(),
                )?;
                let k = if extword.dest_fmt() == 0b011 {
                    extword.k_factor()
                } else {
                    // Only the low seven bits count (MC68881UM FMOVE)
                    let k = self
                        .regs
                        .read_d::<Byte>(extword.k_factor() as usize >> 4 & 0b111);
                    ((k << 1) as i8) >> 1
                };
                self.advance_cycles(cycles)?;
                self.write_fpu_packed(ea, self.regs.fpu.fp[fpx], k)?;
            }
            _ => bail!(CpuError::IllegalInstruction),
        }

        let excs = self.regs.fpu.fpsr;
        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
        Ok(())
    }

    /// The end of an FMOVE to memory or a data register of `value`, with the time it
    /// takes, its effective address coming before all that
    fn fpu_store<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        in_dn: bool,
        cycles: Ticks,
        value: T,
    ) -> Result<()> {
        if in_dn {
            self.advance_cycles(cycles)?;
            self.regs.write_d(instr.get_op2(), value);
        } else {
            let ea = self.calc_ea_addr::<T>(instr, instr.get_addr_mode()?, instr.get_op2())?;
            self.advance_cycles(cycles)?;
            self.write_ticks(ea, value)?;
        }
        Ok(())
    }

    /// FMOVEM of the control registers. They are always moved in the same
    /// order, FPCR first, to increasing addresses whatever the mode (MC68881UM
    /// FMOVEM): with -(An), An goes down by all of them first.
    fn op_fmovem_control(&mut self, instr: &Instruction, extword: FmoveExtWord) -> Result<()> {
        let to_ea = extword.subop() == 0b101;
        let regs = || FmoveControlReg::iter().filter(|r| extword.reg() & r.to_u8().unwrap() != 0);
        let mode = instr.get_addr_mode()?;

        if matches!(
            mode,
            AddressingMode::DataRegister
                | AddressingMode::AddressRegister
                | AddressingMode::Immediate
        ) {
            for r in regs() {
                if to_ea {
                    let value = self.regs.read::<Long>(r.into());
                    self.write_ea(instr, instr.get_op2(), value)?;
                } else {
                    let value = self.read_ea::<Long>(instr, instr.get_op2())?;
                    self.regs.write(r.into(), value);
                }
            }
        } else {
            let mut addr = self.calc_ea_addr_no_mod::<Long>(instr, instr.get_op2())?;
            let size = 4 * regs().count() as Address;
            if mode == AddressingMode::IndirectPreDec {
                addr = addr.wrapping_sub(size);
            }
            let base = addr;
            for r in regs() {
                if to_ea {
                    let value = self.regs.read::<Long>(r.into());
                    self.write_ticks(addr, value)?;
                } else {
                    let value = self.read_ticks::<Long>(addr)?;
                    self.regs.write(r.into(), value);
                }
                addr = addr.wrapping_add(4);
            }
            match mode {
                AddressingMode::IndirectPreDec => self.regs.write_a(instr.get_op2(), base),
                AddressingMode::IndirectPostInc => self.regs.write_a(instr.get_op2(), addr),
                _ => (),
            }
        }

        self.advance_cycles(Self::fpu_timings().move_creg(instr, extword.reg(), !to_ea)?)
    }

    /// FMOVEM - Multiple FPU register move
    pub(in crate::cpu_m68k) fn op_fmovem(
        &mut self,
        instr: &Instruction,
        extword: FmoveExtWord,
    ) -> Result<()> {
        let mode = extword.movem_mode();
        let reglist = extword.movem_reglist();

        // The register list is static in bits 7-0, or dynamic in the low
        // byte of the data register of bits 6-4 (M68000PRM FMOVEM)
        let reglist = match mode {
            0b00 | 0b10 => reglist,
            _ => self
                .regs
                .read_d::<Byte>(usize::from((reglist >> 4) & 0b111)),
        };
        // For predecrement mode, iterate in reverse order
        let reverse_mode = mode & 0b10 == 0;

        if !extword.movem_dir() {
            // EA to registers
            self.op_fmovem_ea_to_regs(instr, reglist, reverse_mode)?;
        } else {
            // Registers to EA
            self.op_fmovem_regs_to_ea(instr, reglist, reverse_mode)?;
        }

        Ok(())
    }

    /// FMOVEM registers to EA
    fn op_fmovem_regs_to_ea(
        &mut self,
        instr: &Instruction,
        reglist: u8,
        reverse_order: bool,
    ) -> Result<()> {
        let mut addr = self.calc_ea_addr_no_mod::<Address>(instr, instr.get_op2())?;

        let (setup, per_reg) = Self::fpu_timings().fmovem_to_ea();
        self.advance_cycles(setup)?;

        for fp_reg in fmovem_order(reverse_order) {
            let bit = if reverse_order { fp_reg } else { 7 - fp_reg };
            if reglist & (1 << bit) == 0 {
                continue;
            }

            let fp_value = self.regs.fpu.fp[fp_reg];

            if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                // Predecrement: decrement address before write
                addr = addr.wrapping_sub(12); // Extended precision = 12 bytes
                self.write_fpu_extended(addr, fp_value)?;
            } else {
                // Other modes: write then increment
                self.write_fpu_extended(addr, fp_value)?;
                addr = addr.wrapping_add(12);
            }

            self.advance_cycles(per_reg)?;
        }

        // Update address register for predec/postinc modes
        match instr.get_addr_mode()? {
            AddressingMode::IndirectPreDec | AddressingMode::IndirectPostInc => {
                self.regs.write_a(instr.get_op2(), addr);
            }
            _ => {}
        }

        Ok(())
    }

    /// FMOVEM EA to registers
    fn op_fmovem_ea_to_regs(
        &mut self,
        instr: &Instruction,
        reglist: u8,
        reverse_order: bool,
    ) -> Result<()> {
        let mut addr = self.calc_ea_addr_no_mod::<Address>(instr, instr.get_op2())?;

        let (setup, per_reg) = Self::fpu_timings().fmovem_to_regs();
        self.advance_cycles(setup)?;

        for fp_reg in fmovem_order(reverse_order) {
            let bit = if reverse_order { fp_reg } else { 7 - fp_reg };
            if reglist & (1 << bit) == 0 {
                continue;
            }
            if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                // Predecrement: decrement address before read
                addr = addr.wrapping_sub(12); // Extended precision = 12 bytes
                self.regs.fpu.fp[fp_reg] = self.read_fpu_extended(addr)?;
            } else {
                // Other modes: read then increment
                self.regs.fpu.fp[fp_reg] = self.read_fpu_extended(addr)?;
                addr = addr.wrapping_add(12);
            }

            self.advance_cycles(per_reg)?;
        }

        // Update address register for predec/postinc modes
        match instr.get_addr_mode()? {
            AddressingMode::IndirectPreDec | AddressingMode::IndirectPostInc => {
                self.regs.write_a(instr.get_op2(), addr);
            }
            _ => {}
        }

        Ok(())
    }
}

/// The order in which FMOVEM moves the floating-point data registers: from FP0
/// up to increasing addresses, or from FP7 down to decreasing addresses in the
/// predecrement mode, whose register list has FP7 in bit 7 instead of FP0, so
/// that the lowest numbered register is always at the lowest address
/// (M68000PRM FMOVEM)
fn fmovem_order(predecrement: bool) -> impl Iterator<Item = usize> {
    let up = 0..8;
    if predecrement {
        Either::Left(up.rev())
    } else {
        Either::Right(up)
    }
}
