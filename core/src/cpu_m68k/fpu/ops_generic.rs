use crate::cpu_m68k::FpuM68kType;
use anyhow::{Result, bail};
use arpfloat::Float;
use either::Either;
use num_traits::ToPrimitive;
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::{
    CpuError, CpuM68k, ExceptionGroup, VECTOR_FORMAT_ERROR, VECTOR_PRIVILEGE_VIOLATION,
};
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::regs::FpuRegisterFile;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
use crate::cpu_m68k::{CpuM68kType, FPU_M68881, FPU_M68882};
use crate::types::{Byte, Long, Word};

use super::storage::{DOUBLE_SIZE, EXTENDED_SIZE, PACKED_SIZE, SINGLE_SIZE};

// Cycle counts returned from fpu_alu_op
// [FPn to FPn, integer, single, double, extended, packed]
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_FPN: usize = 0;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_INT: usize = 1;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_SINGLE: usize = 2;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_DOUBLE: usize = 3;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_EXTENDED: usize = 4;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_PACKED: usize = 5;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_LEN: usize = 6;

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
    /// FNOP
    pub(in crate::cpu_m68k) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        self.regs.fpu.used = true;

        // Fetch second word (0000)
        self.fetch()?;

        self.advance_cycles(16)?;

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
            self.advance_cycles(50)?;
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
            _ => todo!(),
        };

        self.advance_cycles(50)?;

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
        } else if state & 0xFF000000 == 0x1F000000 {
            // Idle state frame
            self.regs.fpu.used = true;
            // We've already read 4 bytes
            self.step_ea_addr = None;
            match FPU_TYPE {
                FPU_M68881 => {
                    self.read_ea_sz::<{ 28 - 4 }>(instr, instr.get_op2())?;
                }
                FPU_M68882 => {
                    self.read_ea_sz::<{ 60 - 4 }>(instr, instr.get_op2())?;
                }
                _ => todo!(),
            };
        } else {
            // Not a frame of this FPU: format error, at the FRESTORE with the
            // frame left alone (M68030UM table 8-6)
            log::warn!("Unknown FPU state frame restored: {:08X}", state);
            if let Some(regs) = self.restart_regs.take() {
                regs.restore(&mut self.regs);
            }
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_FORMAT_ERROR, None);
        }

        self.advance_cycles(55)?;

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
                let src = self.regs.fpu.fp[extword.src_spec() as usize].clone();
                let dest = self.regs.fpu.fp[extword.dst_reg()].clone();
                let opmode = extword.opmode();

                let (result, cycles) = self.fpu_alu_op(opmode, &src, &dest)?;
                self.regs.fpu.fp[extword.dst_reg()] = result;
                self.advance_cycles(cycles[FPU_CYCLES_FPN])?;
            }
            0b100 | 0b101 => self.op_fmovem_control(instr, extword)?,
            0b010 => {
                // EA to FPU register, with ALU op
                let fpx = extword.dst_reg();
                let (value_in, cycle_idx) = match extword.src_spec() {
                    0b000 => {
                        // Long
                        (
                            Float::from_i64(
                                SEMANTICS_EXTENDED,
                                self.read_ea::<Long>(instr, instr.get_op2())? as i32 as i64,
                            ),
                            FPU_CYCLES_MEM_INT,
                        )
                    }
                    0b110 => {
                        // Byte
                        (
                            Float::from_i64(
                                SEMANTICS_EXTENDED,
                                self.read_ea::<Byte>(instr, instr.get_op2())? as i8 as i64,
                            ),
                            FPU_CYCLES_MEM_INT,
                        )
                    }
                    0b100 => {
                        // Word
                        (
                            Float::from_i64(
                                SEMANTICS_EXTENDED,
                                self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i64,
                            ),
                            FPU_CYCLES_MEM_INT,
                        )
                    }
                    0b101 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Double-precision real (immediate)
                        (self.read_fpu_double_imm()?, FPU_CYCLES_MEM_DOUBLE)
                    }
                    0b101 => {
                        // Double-precision real
                        let ea = self.calc_ea_addr_sz::<DOUBLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_double(ea)?, FPU_CYCLES_MEM_DOUBLE)
                    }
                    0b001 if instr.get_addr_mode()? == AddressingMode::DataRegister => {
                        // Single-precision real (Dn)
                        (
                            self.read_fpu_single_dn(instr.get_op2())?,
                            FPU_CYCLES_MEM_SINGLE,
                        )
                    }
                    0b001 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Single-precision real (immediate)
                        (self.read_fpu_single_imm()?, FPU_CYCLES_MEM_SINGLE)
                    }
                    0b001 => {
                        // Single-precision real
                        let ea = self.calc_ea_addr_sz::<SINGLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_single(ea)?, FPU_CYCLES_MEM_SINGLE)
                    }
                    0b010 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Extended-precision real (immediate)
                        (self.read_fpu_extended_imm()?, FPU_CYCLES_MEM_EXTENDED)
                    }
                    0b010 => {
                        // Extended-precision real
                        let ea = self.calc_ea_addr_sz::<EXTENDED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_extended(ea)?, FPU_CYCLES_MEM_EXTENDED)
                    }
                    0b111 => {
                        // ROM constant (FMOVECR)
                        // 10^n rounded once from a wider precision, as the
                        // squarings in powi() lose bits from 10^64 up
                        let pow10 = |n| {
                            Float::from_u64(SEMANTICS_EXTENDED.increase_precision(64), 10)
                                .powi(n)
                                .cast(SEMANTICS_EXTENDED)
                        };
                        self.regs.fpu.fp[fpx] = match extword.movecr_offset() {
                            0x00 => Float::pi(SEMANTICS_EXTENDED),
                            0x0B => Float::from_u64(SEMANTICS_EXTENDED, 2).log10(),
                            0x0C => Float::e(SEMANTICS_EXTENDED),
                            0x0D => Float::e(SEMANTICS_EXTENDED).log2(),
                            0x0E => Float::e(SEMANTICS_EXTENDED).log10(),
                            0x0F => Float::zero(SEMANTICS_EXTENDED, false),
                            0x30 => Float::from_u64(SEMANTICS_EXTENDED, 2).log(),
                            0x31 => Float::from_u64(SEMANTICS_EXTENDED, 10).log(),
                            0x32 => Float::from_i64(SEMANTICS_EXTENDED, 1),
                            0x33 => Float::from_i64(SEMANTICS_EXTENDED, 10),
                            0x34 => Float::from_i64(SEMANTICS_EXTENDED, 100),
                            0x35 => Float::from_i64(SEMANTICS_EXTENDED, 10000),
                            0x36 => pow10(8),
                            0x37 => pow10(16),
                            0x38 => pow10(32),
                            0x39 => pow10(64),
                            0x3A => pow10(128),
                            0x3B => pow10(256),
                            0x3C => pow10(512),
                            0x3D => pow10(1024),
                            0x3E => pow10(2048),
                            0x3F => pow10(4096),
                            // Reserved, and different per mask set (68881UM FMOVECR)
                            _ => Float::zero(SEMANTICS_EXTENDED, false),
                        };

                        // No ALU operation
                        // Not sure how many cycles this costs, assuming its cheap
                        let value = self.regs.fpu.fp[fpx].clone();
                        self.fpu_condition_codes(&value);
                        return Ok(());
                    }
                    0b011 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // BCD packed decimal real (immediate)
                        (self.read_fpu_packed_imm()?, FPU_CYCLES_MEM_PACKED)
                    }
                    0b011 => {
                        // BCD packed decimal real
                        let ea = self.calc_ea_addr_sz::<PACKED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        (self.read_fpu_packed(ea)?, FPU_CYCLES_MEM_PACKED)
                    }
                    _ => {
                        bail!(CpuError::IllegalInstruction);
                    }
                };

                let dest = self.regs.fpu.fp[fpx].clone();
                let (result, cycles) = self.fpu_alu_op(extword.opmode(), &value_in, &dest)?;
                self.regs.fpu.fp[fpx] = result;

                // Table 8-2: "If the source or destination is an MPU data register,
                // subtract five or two clock cycles, respectively."
                if instr.get_addr_mode()? == AddressingMode::DataRegister {
                    self.advance_cycles(cycles[cycle_idx] - 5)?;
                } else {
                    self.advance_cycles(cycles[cycle_idx])?;
                }
            }
            0b011 if instr.get_addr_mode()? != AddressingMode::DataRegister => {
                // Register to EA
                let fpx = extword.src_reg();
                self.regs.fpu.fpsr.exs_mut().set_operr(false);
                match extword.dest_fmt() {
                    0b000 => {
                        // Long
                        let ea = self.calc_ea_addr::<Long>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        let out = self.fpu_to_integer(fpx, i32::MIN.into(), i32::MAX.into()) as i32;

                        self.advance_cycles(100)?;
                        self.write_ticks(ea, out as Long)?;
                    }
                    0b100 => {
                        // Word
                        let ea = self.calc_ea_addr::<Word>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        let out = self.fpu_to_integer(fpx, i16::MIN.into(), i16::MAX.into()) as i16;

                        self.advance_cycles(100)?;
                        self.write_ticks(ea, out as Word)?;
                    }
                    0b110 => {
                        // Byte
                        let ea = self.calc_ea_addr::<Byte>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        let out = self.fpu_to_integer(fpx, i8::MIN.into(), i8::MAX.into()) as i8;

                        self.advance_cycles(100)?;
                        self.write_ticks(ea, out as Byte)?;
                    }
                    0b010 => {
                        // Extended-precision real
                        let ea = self.calc_ea_addr_sz::<EXTENDED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        self.advance_cycles(72)?;
                        self.write_fpu_extended(ea, &self.regs.fpu.fp[fpx].clone())?;
                    }
                    0b101 => {
                        // Double-precision real
                        let ea = self.calc_ea_addr_sz::<DOUBLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        self.advance_cycles(86)?;
                        self.write_fpu_double(ea, &self.regs.fpu.fp[fpx].clone())?;
                    }
                    0b001 => {
                        // Single precision real
                        let ea = self.calc_ea_addr_sz::<SINGLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        self.advance_cycles(80)?;
                        self.write_fpu_single(ea, &self.regs.fpu.fp[fpx].clone())?;
                    }
                    0b011 => {
                        // Packed decimal real with static K-factor
                        let ea = self.calc_ea_addr_sz::<PACKED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        self.advance_cycles(80)?; // todo
                        self.write_fpu_packed(
                            ea,
                            &self.regs.fpu.fp[fpx].clone(),
                            extword.k_factor(),
                        )?;
                    }
                    0b111 => {
                        // Packed decimal real with dynamic K-factor
                        let ea = self.calc_ea_addr_sz::<PACKED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        let k = self
                            .regs
                            .read_d::<Byte>(extword.k_factor() as usize >> 4 & 0b111)
                            as i8;
                        self.advance_cycles(80)?; // todo
                        self.write_fpu_packed(ea, &self.regs.fpu.fp[fpx].clone(), k)?;
                    }
                    _ => {
                        bail!(CpuError::IllegalInstruction);
                    }
                }

                // Flags
                self.regs.fpu.fpsr.exs_mut().set_bsun(false);
                self.regs
                    .fpu
                    .fpsr
                    .exs_mut()
                    .set_snan(self.regs.fpu.fp[fpx].is_nan()); // * 1.6.5

                // Condition codes unaffected
            }
            0b011 if instr.get_addr_mode()? == AddressingMode::DataRegister => {
                // Register to Dn
                //
                // Table 8-2: "If the source or destination is an MPU data register,
                // subtract five or two clock cycles, respectively."
                let fpx = extword.src_reg();
                let dn = instr.get_op2();
                self.regs.fpu.fpsr.exs_mut().set_operr(false);
                match extword.dest_fmt() {
                    0b000 => {
                        // Long
                        let out = self.fpu_to_integer(fpx, i32::MIN.into(), i32::MAX.into()) as i32;

                        self.advance_cycles(100 - 2)?;
                        self.regs.write_d(dn, out as Long);
                    }
                    0b100 => {
                        // Word
                        let out = self.fpu_to_integer(fpx, i16::MIN.into(), i16::MAX.into()) as i16;

                        self.advance_cycles(100 - 2)?;
                        self.regs.write_d(dn, out as Word);
                    }
                    0b110 => {
                        // Byte
                        let out = self.fpu_to_integer(fpx, i8::MIN.into(), i8::MAX.into()) as i8;

                        self.advance_cycles(100 - 2)?;
                        self.regs.write_d(dn, out as Byte);
                    }
                    0b001 => {
                        // Single precision real
                        // TODO flags?
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        self.advance_cycles(80 - 2)?;
                        self.regs
                            .write_d(dn, self.regs.fpu.fp[fpx].as_f32().to_bits());
                    }
                    _ => {
                        bail!(CpuError::IllegalInstruction);
                    }
                }

                // Flags
                self.regs.fpu.fpsr.exs_mut().set_bsun(false);
                self.regs
                    .fpu
                    .fpsr
                    .exs_mut()
                    .set_snan(self.regs.fpu.fp[fpx].is_nan()); // * 1.6.5

                // Condition codes unaffected
            }
            0b110 | 0b111 => {
                // FMOVEM - Multiple register move
                self.op_fmovem(instr, extword)?;
            }
            _ => {
                bail!(CpuError::IllegalInstruction);
            }
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

        self.advance_cycles(29)
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

        self.advance_cycles(35)?;

        for fp_reg in fmovem_order(reverse_order) {
            let bit = if reverse_order { fp_reg } else { 7 - fp_reg };
            if reglist & (1 << bit) == 0 {
                continue;
            }

            let fp_value = self.regs.fpu.fp[fp_reg].clone();

            if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                // Predecrement: decrement address before write
                addr = addr.wrapping_sub(12); // Extended precision = 12 bytes
                self.write_fpu_extended(addr, &fp_value)?;
            } else {
                // Other modes: write then increment
                self.write_fpu_extended(addr, &fp_value)?;
                addr = addr.wrapping_add(12);
            }

            // 3 * 4 cycles spent writing (for long-aligned access)
            self.advance_cycles(25 - 12)?;
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

        self.advance_cycles(33)?;

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

            // 3 * 4 cycles spent reading (for long-aligned access)
            self.advance_cycles(31 - 12)?;
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
