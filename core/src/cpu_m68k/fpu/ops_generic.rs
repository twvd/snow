use crate::cpu_m68k::FpuM68kType;
use anyhow::{Result, bail};
use arpfloat::Float;
use either::Either;
use num_traits::ToPrimitive;
use strum::IntoEnumIterator;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::{CpuM68k, PrefetchWord};
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::regs::FpuRegisterFile;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
use crate::cpu_m68k::{CpuM68kType, FPU_M68881, FPU_M68882};
use crate::tickable::Ticks;
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
    /// Spends the time of an arithmetic operation, `total` from the `fmt` column of
    /// the timing tables, less `less`. The CPU only waits for the transfer of the
    /// operation, and the FPU runs the rest on its own, until the next FPU instruction
    /// (MC68881/MC68882UM 5.1.1.1, 8.6). The time the CPU waits for is the total less
    /// the allowed overlap: the numbers of Table 8-25 on the 68881, and the total less
    /// the T column of Table 8-3 on the 68882, the same for every operation.
    ///
    /// The 68882 can also start the next arithmetic operation before the end of this
    /// one: see fpu_head().
    fn fpu_arith_cycles(&mut self, total: Ticks, fmt: usize, less: Ticks) -> Result<()> {
        let cpu: Ticks = match FPU_TYPE {
            FPU_M68882 => [21, 40, 31, 37, 43, 82],
            _ => [3, 22, 22, 28, 34, 34],
        }[fmt];
        let total = total - less;
        let cpu = cpu.saturating_sub(less).min(total);
        self.advance_cycles(cpu)?;
        self.fpu_free_at = self.cycles + total - cpu;
        Ok(())
    }

    /// Returns the head of the arithmetic operation that the general FPU instruction
    /// `opcode` starts, from its extension word at the front of the prefetch queue:
    /// the time at its start that the 68882 overlaps with the end of the previous
    /// operation (MC68881/MC68882UM 8.5.1.3, H column of Table 8-3, about the same
    /// for every operation in a format). Returns 0 on the 68881, which cannot
    /// overlap two operations, and for the other instructions.
    pub(in crate::cpu_m68k) fn fpu_head(&self, opcode: Word) -> Ticks {
        if FPU_TYPE != FPU_M68882 || opcode & 0b0000_0001_1100_0000 != 0 {
            return 0;
        }
        let extword = match self.prefetch.front() {
            Some(PrefetchWord::Word(w)) => FmoveExtWord(*w),
            _ => return 0,
        };
        // [FPm, integer, single, double, extended, packed]
        match (extword.subop(), extword.src_spec()) {
            (0b000, _) => 17,
            (0b010, 0b000 | 0b100 | 0b110) => 21,
            (0b010, 0b001) => 30,
            (0b010, 0b101) => 36,
            (0b010, 0b010) => 42,
            (0b010, 0b011) => 13,
            _ => 0,
        }
    }

    /// FNOP
    pub(in crate::cpu_m68k) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        // Fetch second word (0000)
        self.fetch()?;

        // 18 cycles (MC68881/MC68882UM Table 8-7)
        self.advance_cycles(14)?;

        Ok(())
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn op_fsave(&mut self, instr: &Instruction) -> Result<()> {
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

        // Idle state frame: 52 cycles on the 68881, 100 on the 68882 (MC68881/MC68882UM
        // Table 8-8), with 7 or 15 writes of 3 cycles
        self.advance_cycles(if FPU_TYPE == FPU_M68882 { 53 } else { 29 })?;

        Ok(())
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn op_frestore(&mut self, instr: &Instruction) -> Result<()> {
        let state = self.read_ea::<Long>(instr, instr.get_op2())?;

        // 21 cycles for a null state frame, 57 for an idle one on the 68881 and 105 on the
        // 68882 (MC68881/MC68882UM Table 8-8), with 1, 7 or 15 reads of 3 cycles
        if state & 0xFF000000 == 0 {
            // NULL state frame, reset FPU
            self.regs.fpu = FpuRegisterFile::default();
            self.advance_cycles(16)?;
        } else if state & 0xFF000000 == 0x1F000000 {
            // Idle state frame
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
            self.advance_cycles(if FPU_TYPE == FPU_M68882 { 58 } else { 34 })?;
        } else {
            bail!("Unknown FPU state frame restored: {:08X}", state);
        }

        Ok(())
    }

    /// FMOVE, ALU operations
    pub(in crate::cpu_m68k) fn op_f000(&mut self, instr: &Instruction) -> Result<()> {
        // Fetch extension word
        let extword = FmoveExtWord(self.fetch_pump()?);
        let m68882 = FPU_TYPE == FPU_M68882;

        match extword.subop() {
            0b000 => {
                // Data reg to data reg, with ALU
                let src = self.regs.fpu.fp[extword.src_spec() as usize].clone();
                let dest = self.regs.fpu.fp[extword.dst_reg()].clone();
                let opmode = extword.opmode();

                let (result, cycles) = self.fpu_alu_op(opmode, &src, &dest)?;
                self.regs.fpu.fp[extword.dst_reg()] = result;
                self.fpu_arith_cycles(cycles[FPU_CYCLES_FPN], FPU_CYCLES_FPN, 0)?;
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

                self.advance_cycles(Self::fmove_ctrl_idle(instr, extword.reg(), true)?)?;
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

                self.advance_cycles(Self::fmove_ctrl_idle(instr, extword.reg(), false)?)?;
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
                            _ => bail!(
                                "Unimplemented FMOVECR offset ${:02X}",
                                extword.movecr_offset()
                            ),
                        };

                        // No ALU operation
                        self.advance_cycles(if m68882 { 32 } else { 29 })?;
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
                let less = if instr.get_addr_mode()? == AddressingMode::DataRegister {
                    5
                } else {
                    0
                };
                self.fpu_arith_cycles(cycles[cycle_idx], cycle_idx, less)?;
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

                        self.advance_cycles(if m68882 { 110 } else { 100 })?;
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

                        self.advance_cycles(if m68882 { 110 } else { 100 })?;
                        self.write_ticks(ea, out as Word)?;
                    }
                    0b110 => {
                        // Byte
                        let ea = self.calc_ea_addr::<Byte>(
                            instr,
                            instr.get_addr_mode()?,
                            instr.get_op2(),
                        )?;
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);

                        let out64 = self.regs.fpu.fp[fpx].to_i64();
                        let (out, inex) = if out64 > i8::MAX.into() {
                            // Overflow
                            (i8::MAX, true)
                        } else if out64 < i8::MIN.into() {
                            // Underflow
                            (i8::MIN, true)
                        } else {
                            // We're good
                            (out64 as i8, false)
                        };
                        self.regs.fpu.fpsr.exs_mut().set_inex2(inex);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(inex);

                        self.advance_cycles(if m68882 { 110 } else { 100 })?;
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

                        self.advance_cycles(if m68882 { 50 } else { 72 })?;
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

                        self.advance_cycles(if m68882 { 44 } else { 86 })?;
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

                        self.advance_cycles(if m68882 { 38 } else { 80 })?;
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

                        self.advance_cycles(if m68882 { 2006 } else { 2002 })?;
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
                        // 14 more for a dynamic k factor
                        self.advance_cycles(if m68882 { 2020 } else { 2016 })?;
                        self.write_fpu_packed(ea, &self.regs.fpu.fp[fpx].clone(), k)?;
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

                        self.advance_cycles(if m68882 { 110 - 2 } else { 100 - 2 })?;
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

                        self.advance_cycles(if m68882 { 110 - 2 } else { 100 - 2 })?;
                        self.regs.write_d(dn, out as Word);
                    }
                    0b110 => {
                        // Byte
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);

                        let out64 = self.regs.fpu.fp[fpx].to_i64();
                        let (out, inex) = if out64 > i8::MAX.into() {
                            // Overflow
                            (i8::MAX, true)
                        } else if out64 < i8::MIN.into() {
                            // Underflow
                            (i8::MIN, true)
                        } else {
                            // We're good
                            (out64 as i8, false)
                        };
                        self.regs.fpu.fpsr.exs_mut().set_inex2(inex);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(inex);

                        self.advance_cycles(if m68882 { 110 - 2 } else { 100 - 2 })?;
                        self.regs.write_d(dn, out as Byte);
                    }
                    0b001 => {
                        // Single precision real
                        // TODO flags?
                        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex2(false);
                        self.regs.fpu.fpsr.exs_mut().set_inex1(false);

                        self.advance_cycles(if m68882 { 38 - 2 } else { 80 - 2 })?;
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

    /// Idle cycles of FMOVE and FMOVEM of control registers: the times of
    /// MC68881/MC68882UM Table 8-6 less the 2 instruction words, the immediate data
    /// and the operand bus cycles of 3 cycles.
    fn fmove_ctrl_idle(instr: &Instruction, regs: u8, to_fpu: bool) -> Result<Ticks> {
        let n = Ticks::from(regs.count_ones());
        let mode = instr.get_addr_mode()?;
        let register = matches!(
            mode,
            AddressingMode::DataRegister | AddressingMode::AddressRegister
        );
        Ok(match (n, to_fpu) {
            // FMOVE: 28 from Rn, 30 from #<data>, 33 from <ea>, 31 to Rn, 33 to <ea>
            (1, true) if register => 24,
            (1, true) if mode == AddressingMode::Immediate => 22,
            (1, false) if register => 27,
            (1, _) => 26,
            // FMOVEM: 25 + 6n from #<data>, 27 + 6n otherwise
            (_, true) if mode == AddressingMode::Immediate => 21 + 2 * n,
            _ => 23 + 3 * n,
        })
    }

    /// FMOVEM registers to EA
    fn op_fmovem_regs_to_ea(
        &mut self,
        instr: &Instruction,
        reglist: u8,
        reverse_order: bool,
    ) -> Result<()> {
        let mut addr = self.calc_ea_addr_no_mod::<Address>(instr, instr.get_op2())?;

        // 37 + 25n cycles (MC68881/MC68882UM Table 8-6), with 3 writes of 3 cycles
        // for each register
        self.advance_cycles(33)?;

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

            self.advance_cycles(25 - 9)?;
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

        // 35 + 31n cycles (MC68881/MC68882UM Table 8-6), with 3 reads of 3 cycles
        // for each register
        self.advance_cycles(31)?;

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

            self.advance_cycles(31 - 9)?;
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
