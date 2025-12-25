//! M68k CPU - Effective Address / Addressing modes handling

use crate::cpu_m68k::FpuM68kType;
use anyhow::{bail, Result};
use arrayvec::ArrayVec;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::instruction::MemoryIndirectAction;
use crate::types::Long;

use super::cpu::CpuM68k;
use super::instruction::{AddressingMode, IndexSize, Instruction, Xn};
use super::{CpuM68kType, CpuSized, M68020, TORDER_HIGHLOW};

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
    /// Calculates address from effective addressing mode, based on operand type
    #[inline(always)]
    pub(in crate::cpu_m68k) fn calc_ea_addr<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
    ) -> Result<Address> {
        self.calc_ea_addr_ex::<T, false>(instr, addrmode, ea_in)
    }

    /// Calculates address from effective addressing mode, based on operand type
    pub(in crate::cpu_m68k) fn calc_ea_addr_ex<T: CpuSized, const HOLD: bool>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
    ) -> Result<Address> {
        // TODO Waiting for generic_const_exprs to stabilize..
        // https://github.com/rust-lang/rust/issues/76560
        match std::mem::size_of::<T>() {
            1 => self.calc_ea_addr_sz_ex::<1, HOLD>(instr, addrmode, ea_in),
            2 => self.calc_ea_addr_sz_ex::<2, HOLD>(instr, addrmode, ea_in),
            4 => self.calc_ea_addr_sz_ex::<4, HOLD>(instr, addrmode, ea_in),
            _ => unreachable!(),
        }
    }

    /// Calculates effective address with an arbitrary type size.
    /// Applies pre-decrement and post-increment
    /// Happens once per instruction so e.g. postinc/predec only occur once.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn calc_ea_addr_sz<const SZ: usize>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
    ) -> Result<Address> {
        self.calc_ea_addr_sz_ex::<SZ, false>(instr, addrmode, ea_in)
    }

    /// Calculates effective address with an arbitrary type size.
    /// Applies pre-decrement and post-increment
    /// Happens once per instruction so e.g. postinc/predec only occur once.
    pub(in crate::cpu_m68k) fn calc_ea_addr_sz_ex<const SZ: usize, const HOLD: bool>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
    ) -> Result<Address> {
        if let Some(addr) = self.step_ea_addr {
            // Already done this step
            return Ok(addr);
        }

        let read_idx = |s: &Self, (xn, reg): (Xn, usize), size: IndexSize| {
            let v = match xn {
                Xn::Dn => s.regs.read_d(reg),
                Xn::An => s.regs.read_a(reg),
            };
            match size {
                IndexSize::Word => v as u16 as i16 as i32 as u32,
                IndexSize::Long => v,
            }
        };
        let addr = match addrmode {
            AddressingMode::DataRegister => bail!("calc_ea_addr invalid addressing mode Dn"),
            AddressingMode::AddressRegister => bail!("calc_ea_addr invalid addressing mode An"),
            AddressingMode::Indirect => self.regs.read_a(ea_in),
            AddressingMode::IndirectPreDec => {
                self.advance_cycles(2)?; // 2x idle
                self.regs.read_a_predec::<Address>(ea_in, SZ)
            }
            AddressingMode::IndirectPostInc => {
                let addr = self.regs.read_a::<Address>(ea_in);
                let inc_addr = if ea_in == 7 {
                    // Minimum of 2 for A7
                    addr.wrapping_add(std::cmp::max(2, SZ as Address))
                } else {
                    addr.wrapping_add(SZ as Address)
                };
                if !HOLD {
                    self.regs.write_a::<Address>(ea_in, inc_addr);
                } else {
                    self.step_ea_load = Some((ea_in, inc_addr));
                }
                addr
            }
            AddressingMode::IndirectDisplacement => {
                instr.fetch_extword(|| self.fetch_pump())?;
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = instr.get_displacement();
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::IndirectIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extword(|| self.fetch_pump())?;

                let extword = instr.get_extword();
                if extword.is_full() && CPU_TYPE >= M68020 {
                    // Actually IndirectIndexBase
                    return self.calc_ea_addr_sz_ex::<SZ, HOLD>(
                        instr,
                        AddressingMode::IndirectIndexBase,
                        ea_in,
                    );
                }
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                let scale = if CPU_TYPE >= M68020 {
                    extword.brief_get_scale()
                } else {
                    1
                };

                addr.wrapping_add(displacement)
                    .wrapping_add(index.wrapping_mul(Address::from(scale)))
            }
            AddressingMode::PCDisplacement => {
                instr.fetch_extword(|| self.fetch_pump())?;
                let addr = self.regs.pc;
                let displacement = instr.get_displacement();
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::PCIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extword(|| self.fetch_pump())?;
                let extword = instr.get_extword();
                if extword.is_full() && CPU_TYPE >= M68020 {
                    return self.calc_ea_addr_sz_ex::<SZ, HOLD>(
                        instr,
                        AddressingMode::PCIndexBase,
                        ea_in,
                    );
                }
                let pc = self.regs.pc;
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                let scale = if CPU_TYPE >= M68020 {
                    extword.brief_get_scale()
                } else {
                    1
                };
                pc.wrapping_add(displacement)
                    .wrapping_add(index.wrapping_mul(Address::from(scale)))
            }
            AddressingMode::AbsoluteShort => self.fetch_pump()? as i16 as i32 as u32,
            AddressingMode::AbsoluteLong => {
                let h = self.fetch_pump()? as u32;
                let l = self.fetch_pump()? as u32;
                (h << 16) | l
            }
            AddressingMode::Immediate => {
                bail!("Invalid addressing mode at PC {:08X}", self.regs.pc)
            }
            AddressingMode::IndirectIndexBase | AddressingMode::PCIndexBase => {
                // also Memory Indirect modes
                // TODO cycles?
                debug_assert!(instr.has_extword());
                let extword = instr.get_extword();

                let addr = if extword.full_base_suppress() {
                    0
                } else {
                    match addrmode {
                        AddressingMode::IndirectIndexBase => self.regs.read_a::<Address>(ea_in),
                        AddressingMode::PCIndexBase => self.regs.pc,
                        _ => unreachable!(),
                    }
                };
                let displacement = instr.fetch_ind_full_displacement(|| self.fetch_pump())?;
                let scale = extword.full_scale();

                let index = if let Some(idxreg) = extword.full_index_register() {
                    read_idx(self, idxreg.into(), extword.full_index_size())
                } else {
                    // Index suppressed, leave at 0 for no effect
                    0
                };
                let disp_addr = addr.wrapping_add_signed(displacement);
                let pre_addr = disp_addr.wrapping_add(index.wrapping_mul(u32::from(scale)));

                match extword.full_memindirectmode()? {
                    MemoryIndirectAction::None => {
                        // Address Register Indirect with Index (Base Displacement) Mode
                        pre_addr
                    }
                    MemoryIndirectAction::Null => {
                        // Memory Indirect (no index)
                        self.read_ticks(disp_addr)?
                    }
                    MemoryIndirectAction::Word => {
                        let od = self.fetch_pump()?.expand_sign_extend();

                        self.read_ticks::<Address>(disp_addr)?
                            .wrapping_add_signed(od as i32)
                    }
                    MemoryIndirectAction::Long => {
                        let mut od = Long::from(self.fetch_pump()?) << 16;
                        od |= Long::from(self.fetch_pump()?);
                        self.read_ticks::<Address>(disp_addr)?
                            .wrapping_add_signed(od as i32)
                    }
                    MemoryIndirectAction::PostIndexNull => self
                        .read_ticks::<Address>(disp_addr)?
                        .wrapping_add(index.wrapping_mul(u32::from(scale))),
                    MemoryIndirectAction::PostIndexWord => {
                        let od = self.fetch_pump()?.expand_sign_extend();
                        self.read_ticks::<Address>(disp_addr)?
                            .wrapping_add(index.wrapping_mul(u32::from(scale)))
                            .wrapping_add_signed(od as i32)
                    }
                    MemoryIndirectAction::PostIndexLong => {
                        let mut od = Long::from(self.fetch_pump()?) << 16;
                        od |= Long::from(self.fetch_pump()?);
                        self.read_ticks::<Address>(disp_addr)?
                            .wrapping_add(index.wrapping_mul(u32::from(scale)))
                            .wrapping_add_signed(od as i32)
                    }
                    MemoryIndirectAction::PreIndexNull => self.read_ticks(pre_addr)?,
                    MemoryIndirectAction::PreIndexWord => {
                        let od = self.fetch_pump()?.expand_sign_extend();

                        self.read_ticks::<Address>(pre_addr)?
                            .wrapping_add_signed(od as i32)
                    }
                    MemoryIndirectAction::PreIndexLong => {
                        let mut od = Long::from(self.fetch_pump()?) << 16;
                        od |= Long::from(self.fetch_pump()?);

                        self.read_ticks::<Address>(pre_addr)?
                            .wrapping_add_signed(od as i32)
                    }
                }
            }
        };

        self.step_ea_addr = Some(addr);
        Ok(addr)
    }

    /// Calculates effective address but does not modify any registers in predec/postinc
    pub(in crate::cpu_m68k) fn calc_ea_addr_no_mod<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
    ) -> Result<Address> {
        if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
            // calc_ea_addr() already decrements the address once, but in this case,
            // we don't want that.
            Ok(self.regs.read_a(instr.get_op2()))
        } else {
            let result = self.calc_ea_addr_ex::<T, true>(instr, instr.get_addr_mode()?, ea_in);
            self.step_ea_load = None;
            result
        }
    }

    pub(in crate::cpu_m68k) fn fetch_immediate<T: CpuSized>(&mut self) -> Result<T> {
        Ok(match std::mem::size_of::<T>() {
            1 | 2 => T::chop(self.fetch_pump()?.into()),
            4 => {
                let h = self.fetch_pump()? as u32;
                let l = self.fetch_pump()? as u32;
                T::chop((h << 16) | l)
            }
            _ => unreachable!(),
        })
    }

    /// Commits a postponed 'held' change to address register.
    pub(in crate::cpu_m68k) fn ea_commit(&mut self) {
        if let Some((reg, val)) = self.step_ea_load {
            // Postponed An write from post-increment mode
            self.regs.write_a(reg, val);
        }
        self.step_ea_load = None;
    }

    /// Reads a value from the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ea<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
    ) -> Result<T> {
        self.read_ea_with::<T, false>(instr, instr.get_addr_mode()?, ea_in)
    }

    /// Reads a value from the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    /// Holds off on postincrement.
    #[inline(always)]
    pub(in crate::cpu_m68k) fn read_ea_hold<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
    ) -> Result<T> {
        self.read_ea_with::<T, true>(instr, instr.get_addr_mode()?, ea_in)
    }

    pub(in crate::cpu_m68k) fn read_ea_with<T: CpuSized, const HOLD: bool>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
    ) -> Result<T> {
        let v = match addrmode {
            AddressingMode::DataRegister => self.regs.read_d(ea_in),
            AddressingMode::AddressRegister => self.regs.read_a(ea_in),
            AddressingMode::Immediate => self.fetch_immediate::<T>()?,
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::PCDisplacement
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectPreDec => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectPostInc => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectIndexBase
            | AddressingMode::IndirectIndex
            | AddressingMode::PCIndexBase
            | AddressingMode::PCIndex => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
        };

        Ok(v)
    }

    /// Writes a value to the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    pub(in crate::cpu_m68k) fn write_ea<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
        value: T,
    ) -> Result<()> {
        self.write_ea_with::<T, false, TORDER_HIGHLOW>(instr, instr.get_addr_mode()?, ea_in, value)
    }

    /// Writes a value to the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    #[allow(dead_code)]
    pub(in crate::cpu_m68k) fn write_ea_hold<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
        value: T,
    ) -> Result<()> {
        self.write_ea_with::<T, true, TORDER_HIGHLOW>(instr, instr.get_addr_mode()?, ea_in, value)
    }

    pub(in crate::cpu_m68k) fn write_ea_with<T: CpuSized, const HOLD: bool, const TORDER: usize>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
        value: T,
    ) -> Result<()> {
        match addrmode {
            AddressingMode::DataRegister => Ok(self.regs.write_d(ea_in, value)),
            AddressingMode::AddressRegister => Ok(self.regs.write_a(ea_in, value)),
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectIndex
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.write_ticks_order::<T, TORDER>(addr, value)
            }
            AddressingMode::IndirectPreDec => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.write_ticks_order::<T, TORDER>(addr, value)
            }
            AddressingMode::IndirectPostInc => {
                let addr = self.calc_ea_addr_ex::<T, HOLD>(instr, addrmode, ea_in)?;
                self.write_ticks_order::<T, TORDER>(addr, value)
            }
            _ => {
                bail!("Unimplemented addressing mode: {:?}", addrmode)
            }
        }
    }

    /// Write to effective address a specified number of bytes
    pub(in crate::cpu_m68k) fn write_ea_sz<const SZ: usize>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
        value: [u8; SZ],
    ) -> Result<()> {
        let ea = self.calc_ea_addr_sz::<SZ>(instr, instr.get_addr_mode()?, ea_in)?;
        for (i, b) in value.into_iter().enumerate() {
            self.write_ticks(ea.wrapping_add(i as Address), b)?;
        }

        Ok(())
    }

    /// Read from effective address a specified number of bytes
    pub(in crate::cpu_m68k) fn read_ea_sz<const SZ: usize>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
    ) -> Result<[u8; SZ]> {
        let ea = self.calc_ea_addr_sz::<SZ>(instr, instr.get_addr_mode()?, ea_in)?;
        let mut v = ArrayVec::<u8, SZ>::new();

        for i in 0..SZ {
            v.push(self.read_ticks(ea.wrapping_add(i as Address))?);
        }

        Ok(v.into_inner().unwrap())
    }
}
