use anyhow::{bail, Result};
use arpfloat::Float;
use either::Either;
use num_traits::ToPrimitive;
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
use crate::cpu_m68k::CpuM68kType;
use crate::types::{Byte, Long, Word};

use super::storage::{DOUBLE_SIZE, EXTENDED_SIZE, SINGLE_SIZE};

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// FNOP
    pub(in crate::cpu_m68k) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        // Fetch second word (0000)
        self.fetch()?;

        Ok(())
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn op_fsave(&mut self, instr: &Instruction) -> Result<()> {
        // Idle state frame
        // 0x1F = 68881
        self.write_ea(instr, instr.get_op2(), 0x1F180000u32)?;

        Ok(())
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn op_frestore(&mut self, instr: &Instruction) -> Result<()> {
        let state = self.read_ea::<Long>(instr, instr.get_op2())?;
        if state != 0 && state != 0x1F180000 {
            log::warn!("TODO FPU state frame restored: {:08X}", state);
        }

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

                self.regs.fpu.fp[extword.dst_reg()] = self.fpu_alu_op(opmode, &src, &dest)?;
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
            }
            0b010 => {
                // EA to FPU register, with ALU op
                let fpx = extword.dst_reg();
                let value_in = match extword.src_spec() {
                    0b000 => {
                        // Long
                        Float::from_i64(
                            SEMANTICS_EXTENDED,
                            self.read_ea::<Long>(instr, instr.get_op2())? as i32 as i64,
                        )
                    }
                    0b110 => {
                        // Byte
                        Float::from_i64(
                            SEMANTICS_EXTENDED,
                            self.read_ea::<Byte>(instr, instr.get_op2())? as i8 as i64,
                        )
                    }
                    0b100 => {
                        // Word
                        Float::from_i64(
                            SEMANTICS_EXTENDED,
                            self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i64,
                        )
                    }
                    0b101 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Double-precision real (immediate)
                        self.read_fpu_double_imm()?
                    }
                    0b101 => {
                        // Double-precision real
                        let ea = self.calc_ea_addr_sz::<DOUBLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.read_fpu_double(ea)?
                    }
                    0b001 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Single-precision real (immediate)
                        self.read_fpu_single_imm()?
                    }
                    0b001 => {
                        // Single-precision real
                        let ea = self.calc_ea_addr_sz::<SINGLE_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.read_fpu_single(ea)?
                    }
                    0b010 if instr.get_addr_mode()? == AddressingMode::Immediate => {
                        // Extended-precision real (immediate)
                        self.read_fpu_extended_imm()?
                    }
                    0b010 => {
                        // Extended-precision real
                        let ea = self.calc_ea_addr_sz::<EXTENDED_SIZE>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.read_fpu_extended(ea)?
                    }
                    0b111 => {
                        // ROM constant (FMOVECR)
                        self.regs.fpu.fp[fpx] = match extword.movecr_offset() {
                            0x00 => Float::pi(SEMANTICS_EXTENDED),
                            // TODO 0x0B log10(2)
                            0x0C => Float::e(SEMANTICS_EXTENDED),
                            // TODO 0x0D log2(e)
                            // TODO 0x0E log10(e)
                            0x0F => Float::zero(SEMANTICS_EXTENDED, false),
                            // TODO 0x30 ln(2)
                            // TODO 0x31 ln(10)
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
                self.regs.fpu.fp[fpx] = self.fpu_alu_op(extword.opmode(), &value_in, &dest)?;
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
                        self.regs.write_d(dn, out as Word);
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

                if mode == 0b00 && instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
                    bail!("Contradicting modes (pre-dec vs {0:2b})", mode);
                }
                if mode == 0b10 && instr.get_addr_mode()? != AddressingMode::IndirectPostInc {
                    bail!("Contradicting modes (post-inc vs {0:2b})", mode);
                }

                if !extword.movem_dir() {
                    // EA to registers
                    self.op_fmovem_ea_to_regs(instr, reglist)?;
                } else {
                    // Registers to EA
                    self.op_fmovem_regs_to_ea(instr, reglist)?;
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
    fn op_fmovem_regs_to_ea(&mut self, instr: &Instruction, reglist: u8) -> Result<()> {
        let mut addr = self.calc_ea_addr_no_mod::<Address>(instr, instr.get_op2())?;

        // For predecrement mode, iterate in reverse order
        let reverse_order = instr.get_addr_mode()? == AddressingMode::IndirectPreDec;

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
    fn op_fmovem_ea_to_regs(&mut self, instr: &Instruction, reglist: u8) -> Result<()> {
        let mut addr = self.calc_ea_addr_no_mod::<Address>(instr, instr.get_op2())?;

        // For predecrement mode, iterate in reverse order
        let reverse_order = instr.get_addr_mode()? == AddressingMode::IndirectPreDec;
        let range = if !reverse_order {
            Either::Left((0..8).rev())
        } else {
            Either::Right(0..8)
        };

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
