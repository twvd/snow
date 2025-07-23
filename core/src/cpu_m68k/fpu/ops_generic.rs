use anyhow::{bail, Result};
use arpfloat::Float;
use either::Either;
use num_traits::ToPrimitive;
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
use crate::cpu_m68k::CpuM68kType;
use crate::types::{Byte, Long, Word};

use super::storage::{DOUBLE_SIZE, EXTENDED_SIZE, SINGLE_SIZE};

// Cycle counts returned from fpu_alu_op
// [FPn to FPn, integer, single, double, extended, packed]
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_FPN: usize = 0;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_INT: usize = 1;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_SINGLE: usize = 2;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_DOUBLE: usize = 3;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_EXTENDED: usize = 4;
#[allow(dead_code)]
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_MEM_PACKED: usize = 5;
pub(in crate::cpu_m68k::fpu) const FPU_CYCLES_LEN: usize = 6;

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// FNOP
    pub(in crate::cpu_m68k) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        // Fetch second word (0000)
        self.fetch()?;

        self.advance_cycles(16)?;

        Ok(())
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn op_fsave(&mut self, instr: &Instruction) -> Result<()> {
        // Idle state frame
        // 0x1F = 68881
        self.write_ea(instr, instr.get_op2(), 0x1F180000u32)?;

        self.advance_cycles(50)?;

        Ok(())
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn op_frestore(&mut self, instr: &Instruction) -> Result<()> {
        let state = self.read_ea::<Long>(instr, instr.get_op2())?;
        if state != 0 && state != 0x1F180000 {
            log::warn!("TODO FPU state frame restored: {:08X}", state);
        }

        self.advance_cycles(55)?;

        Ok(())
    }

    /// FMOVE, ALU operations
    pub(in crate::cpu_m68k) fn op_f000(&mut self, instr: &Instruction) -> Result<()> {
        // Fetch extension word
        let extword = FmoveExtWord(self.fetch_pump()?);

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
            0b100 => {
                // From EA to control reg
                // Supports multiple registers
                for r in FmoveControlReg::iter() {
                    if extword.reg() & r.to_u8().unwrap() == 0 {
                        continue;
                    }
                    let value = self.read_ea::<Long>(instr, instr.get_op2())?;
                    self.regs.write(r.into(), value);

                    // Next read from its own, re-calculated address
                    self.ea_commit();
                    self.step_ea_addr = None;
                }

                self.advance_cycles(29)?;
            }
            0b101 => {
                // From control reg to EA
                // Supports multiple registers
                for r in FmoveControlReg::iter() {
                    if extword.reg() & r.to_u8().unwrap() == 0 {
                        continue;
                    }
                    let value = self.regs.read::<Long>(r.into());
                    self.write_ea(instr, instr.get_op2(), value)?;

                    // Next write to its own, re-calculated address
                    self.ea_commit();
                    self.step_ea_addr = None;
                }

                self.advance_cycles(29)?;
            }
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
                            0x36 => Float::one(SEMANTICS_EXTENDED, false).powi(8),
                            0x37 => Float::one(SEMANTICS_EXTENDED, false).powi(16),
                            0x38 => Float::one(SEMANTICS_EXTENDED, false).powi(32),
                            0x39 => Float::one(SEMANTICS_EXTENDED, false).powi(64),
                            0x3A => Float::one(SEMANTICS_EXTENDED, false).powi(128),
                            0x3B => Float::one(SEMANTICS_EXTENDED, false).powi(256),
                            0x3C => Float::one(SEMANTICS_EXTENDED, false).powi(512),
                            0x3D => Float::one(SEMANTICS_EXTENDED, false).powi(1024),
                            0x3E => Float::one(SEMANTICS_EXTENDED, false).powi(2048),
                            0x3F => Float::one(SEMANTICS_EXTENDED, false).powi(4096),
                            _ => bail!(
                                "Unimplemented FMOVECR offset ${:02X}",
                                extword.movecr_offset()
                            ),
                        };

                        // No ALU operation
                        // Not sure how many cycles this costs, assuming its cheap
                        return Ok(());
                    }
                    _ => {
                        bail!(
                            "EA to reg unimplemented src spec {:03b}",
                            extword.src_spec()
                        );
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
                match extword.dest_fmt() {
                    0b000 => {
                        // Long
                        let ea = self.calc_ea_addr::<Long>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);

                        let out64 = self.regs.fpu.fp[fpx].to_i64();
                        let (out, inex) = if out64 > i32::MAX.into() {
                            // Overflow
                            (i32::MAX, true)
                        } else if out64 < i32::MIN.into() {
                            // Underflow
                            (i32::MIN, true)
                        } else {
                            // We're good
                            (out64 as i32, false)
                        };
                        self.regs.fpu.fpsr.exs_mut().set_inex2(inex);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(inex);

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
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);

                        let out64 = self.regs.fpu.fp[fpx].to_i64();
                        let (out, inex) = if out64 > i16::MAX.into() {
                            // Overflow
                            (i16::MAX, true)
                        } else if out64 < i16::MIN.into() {
                            // Underflow
                            (i16::MIN, true)
                        } else {
                            // We're good
                            (out64 as i16, false)
                        };
                        self.regs.fpu.fpsr.exs_mut().set_inex2(inex);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(inex);

                        self.advance_cycles(100)?;
                        self.write_ticks(ea, out as Word)?;
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
                    _ => {
                        bail!(
                            "Reg to EA unimplemented dest format {:03b}",
                            extword.dest_fmt()
                        );
                    }
                }

                // Flags
                self.regs.fpu.fpsr.exs_mut().set_bsun(false);
                self.regs
                    .fpu
                    .fpsr
                    .exs_mut()
                    .set_snan(self.regs.fpu.fp[fpx].is_nan()); // * 1.6.5
                self.regs.fpu.fpsr.exs_mut().set_operr(false); // for invalid K-factor

                // Condition codes unaffected
            }
            0b011 if instr.get_addr_mode()? == AddressingMode::DataRegister => {
                // Register to Dn
                //
                // Table 8-2: "If the source or destination is an MPU data register,
                // subtract five or two clock cycles, respectively."
                let fpx = extword.src_reg();
                let dn = instr.get_op2();
                match extword.dest_fmt() {
                    0b000 => {
                        // Long
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);

                        let out64 = self.regs.fpu.fp[fpx].to_i64();
                        let (out, inex) = if out64 > i32::MAX.into() {
                            // Overflow
                            (i32::MAX, true)
                        } else if out64 < i32::MIN.into() {
                            // Underflow
                            (i32::MIN, true)
                        } else {
                            // We're good
                            (out64 as i32, false)
                        };
                        self.regs.fpu.fpsr.exs_mut().set_inex2(inex);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(inex);

                        self.advance_cycles(100 - 2)?;
                        self.regs.write_d(dn, out as Long);
                    }
                    0b100 => {
                        // Word
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);

                        let out64 = self.regs.fpu.fp[fpx].to_i64();
                        let (out, inex) = if out64 > i16::MAX.into() {
                            // Overflow
                            (i16::MAX, true)
                        } else if out64 < i16::MIN.into() {
                            // Underflow
                            (i16::MIN, true)
                        } else {
                            // We're good
                            (out64 as i16, false)
                        };
                        self.regs.fpu.fpsr.exs_mut().set_inex2(inex);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(inex);

                        self.advance_cycles(100 - 2)?;
                        self.regs.write_d(dn, out as Word);
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
                        bail!(
                            "Reg to Dn unimplemented dest format {:03b}",
                            extword.dest_fmt()
                        );
                    }
                }

                // Flags
                self.regs.fpu.fpsr.exs_mut().set_bsun(false);
                self.regs
                    .fpu
                    .fpsr
                    .exs_mut()
                    .set_snan(self.regs.fpu.fp[fpx].is_nan()); // * 1.6.5
                self.regs.fpu.fpsr.exs_mut().set_operr(false); // for invalid K-factor

                // Condition codes unaffected
            }
            0b110 | 0b111 => {
                // FMOVEM - Multiple register move
                self.op_fmovem(instr, extword)?;
            }
            _ => {
                bail!("Unimplemented sub-operation {:03b}", extword.subop());
            }
        }

        Ok(())
    }

    /// FMOVEM - Multiple FPU register move
    pub(in crate::cpu_m68k) fn op_fmovem(
        &mut self,
        instr: &Instruction,
        extword: FmoveExtWord,
    ) -> Result<()> {
        let mode = extword.movem_mode();
        let reglist = extword.movem_reglist();

        match mode {
            0b00 | 0b10 => {
                // Static register list
                // For predecrement mode, iterate in reverse order
                let reverse_mode = mode & 0b10 == 0;

                if !extword.movem_dir() {
                    // EA to registers
                    self.op_fmovem_ea_to_regs(instr, reglist, reverse_mode)?;
                } else {
                    // Registers to EA
                    self.op_fmovem_regs_to_ea(instr, reglist, reverse_mode)?;
                }
            }
            0b01 | 0b11 => {
                // Dynamic register list (from control register)
                bail!("Dynamic FMOVEM register list not implemented");
            }
            _ => {
                bail!("Invalid FMOVEM mode {:02b}", mode);
            }
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

        let range = if !reverse_order {
            Either::Left((0..8).rev())
        } else {
            Either::Right(0..8)
        };
        for (bit, fp_reg) in range.enumerate() {
            if reglist & (1 << bit as u8) == 0 {
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

        let range = if !reverse_order {
            Either::Left((0..8).rev())
        } else {
            Either::Right(0..8)
        };

        self.advance_cycles(33)?;

        for (bit, fp_reg) in range.enumerate() {
            if reglist & (1 << bit as u8) == 0 {
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
