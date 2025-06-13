use anyhow::{bail, Context, Result};
use arpfloat::Float;
use either::Either;
use num::FromPrimitive;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
use crate::cpu_m68k::CpuM68kType;
use crate::types::{Long, Word};

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
        self.write_ea(instr, instr.get_op2(), 0x00180018u32)?;

        Ok(())
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn op_frestore(&mut self, instr: &Instruction) -> Result<()> {
        let state = self.read_ea::<Long>(instr, instr.get_op2())?;
        if state != 0 {
            log::warn!("TODO FPU state frame restored: {:08X}", state);
        }

        Ok(())
    }

    /// FMOVE
    pub(in crate::cpu_m68k) fn op_fmove(&mut self, instr: &Instruction) -> Result<()> {
        // Fetch extension word
        let extword = FmoveExtWord(self.fetch()?);
        log::debug!("FMOVE {:04X} {:04X}", instr.data, extword.0);

        match extword.subop() {
            0b000 => {
                // Data reg to data reg
                let src = self.regs.fpu.fp[extword.src_spec() as usize].clone();
                self.regs.fpu.fp[extword.dst_reg()] = src;
            }
            0b100 => {
                // From EA to control reg
                let ctrlreg = FmoveControlReg::from_u8(extword.reg())
                    .context("Invalid register selection field")?;
                let value = self.read_ea::<Long>(instr, instr.get_op2())?;
                self.regs.write(ctrlreg.into(), value);
            }
            0b101 => {
                // From control reg to EA
                let ctrlreg = FmoveControlReg::from_u8(extword.reg())
                    .context("Invalid register selection field")?;
                let value = self.regs.read::<Long>(ctrlreg.into());
                self.write_ea(instr, instr.get_op2(), value)?;
            }
            0b010 => {
                // EA to register
                let fpx = extword.dst_reg();
                let value_in = match extword.src_spec() {
                    0b100 => {
                        // Word
                        Float::from_i64(
                            SEMANTICS_EXTENDED,
                            self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i64,
                        )
                    }
                    0b010 => {
                        // Extended real
                        let addr = if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                            self.regs.read_a_predec(instr.get_op2(), 12)
                        } else {
                            self.calc_ea_addr_no_mod::<Long>(instr, instr.get_op2())?
                        };
                        let v = self.read_fpu_extended(addr)?;
                        if instr.get_addr_mode()? == AddressingMode::IndirectPostInc {
                            self.regs.read_a_postinc::<Address>(instr.get_op2(), 12);
                        }
                        v
                    }
                    _ => {
                        bail!(
                            "EA to reg unimplemented src spec {:03b}",
                            extword.src_spec()
                        );
                    }
                };

                if extword.opmode() != 0 {
                    bail!("TODO rounding precision");
                }

                // Flags
                self.regs.fpu.fpsr.exs_mut().set_bsun(false);
                self.regs
                    .fpu
                    .fpsr
                    .exs_mut()
                    .set_snan(self.regs.fpu.fp[fpx].is_nan()); // * 1.6.5
                self.regs.fpu.fpsr.exs_mut().set_operr(false);
                self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                self.regs.fpu.fpsr.exs_mut().set_unfl(false); // * X denormalized
                self.regs.fpu.fpsr.exs_mut().set_inex2(false); // * L, D, X
                self.regs.fpu.fpsr.exs_mut().set_inex1(false); // * P

                // Condition codes (3.6.2)
                self.regs.fpu.fpsr.set_fpcc_nan(value_in.is_nan());
                self.regs.fpu.fpsr.set_fpcc_i(value_in.is_inf());
                self.regs.fpu.fpsr.set_fpcc_n(value_in.is_negative());
                self.regs.fpu.fpsr.set_fpcc_z(value_in.is_zero());

                log::debug!("in {} = {}", fpx, value_in);
                log::debug!("{:?}", self.regs.fpu.fpsr);
                self.regs.fpu.fp[fpx] = value_in;
            }
            0b011 => {
                // Register to EA
                if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                    self.regs.read_a_predec::<Address>(
                        instr.get_op2(),
                        extword
                            .dest_fmt_instrsz()
                            .context("Unknown dest fmt")?
                            .bytelen(),
                    );
                }
                let ea = self.calc_ea_addr_no_mod::<Address>(instr, instr.get_op2())?;
                if instr.get_addr_mode()? == AddressingMode::IndirectPostInc {
                    self.regs.read_a_postinc::<Address>(
                        instr.get_op2(),
                        extword
                            .dest_fmt_instrsz()
                            .context("Unknown dest fmt")?
                            .bytelen(),
                    );
                }
                let fpx = extword.src_reg();
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
                        self.write_ticks(ea, out as Long)?;
                    }
                    0b010 => {
                        // Extended real
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);
                        self.write_fpu_extended(ea, &self.regs.fpu.fp[fpx].clone())?;
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

                if extword.movem_dir() {
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

        let range = if reverse_order {
            Either::Left(0..8)
        } else {
            Either::Right((0..8).rev())
        };
        for fp_reg in range {
            if reglist & (1 << fp_reg) == 0 {
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

        for fp_reg in 0..8 {
            let bit_pos = if reverse_order { 7 - fp_reg } else { fp_reg };

            if reglist & (1 << bit_pos) != 0 {
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
