use std::collections::VecDeque;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};

use super::instruction::{
    AddressingMode, Direction, IndexSize, Instruction, InstructionMnemonic, Xn,
};
use super::regs::RegisterFile;
use super::{Byte, CpuSized, Long, Word};

/// Motorola 680x0
#[derive(Serialize, Deserialize)]
pub struct CpuM68k<TBus: Bus<Address>> {
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
    pub prefetch: VecDeque<u16>,

    step_ea_addr: Option<Address>,
}

impl<TBus> CpuM68k<TBus>
where
    TBus: Bus<Address>,
{
    pub fn new(bus: TBus) -> Self {
        Self {
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            prefetch: VecDeque::with_capacity(3),
            step_ea_addr: None,
        }
    }

    fn prefetch_pump(&mut self) -> Result<()> {
        if self.prefetch.len() >= 2 {
            return Ok(());
        }

        let fetch_addr = ((self.regs.pc & !1) + 4) & ADDRESS_MASK;
        let new_item = self.read_ticks::<Word>(fetch_addr)?;
        self.prefetch.push_back(new_item);
        self.regs.pc = (self.regs.pc + 2) & ADDRESS_MASK;
        Ok(())
    }

    /// Re-fills the prefetch queue
    fn prefetch_refill(&mut self) -> Result<()> {
        while self.prefetch.len() < 2 {
            self.prefetch_pump()?;
        }
        Ok(())
    }

    /// Fetches a 16-bit value, through the prefetch queue
    fn fetch_pump(&mut self) -> Result<Word> {
        let v = self.prefetch.pop_front().unwrap();
        self.prefetch_pump()?;
        Ok(v)
    }

    /// Fetches a 16-bit value from prefetch queue
    fn fetch(&mut self) -> Result<Word> {
        if self.prefetch.len() == 0 {
            self.prefetch_pump()?;
        }
        Ok(self.prefetch.pop_front().unwrap())
    }

    /// Executes a single CPU step.
    pub fn step(&mut self) -> Result<()> {
        debug_assert_eq!(self.prefetch.len(), 2);

        self.step_ea_addr = None;

        let instr = Instruction::try_decode(|| self.fetch())?;
        // TODO decoded instruction cache

        self.execute_instruction(instr)?;
        self.prefetch_refill()?;
        Ok(())
    }

    /// Advances by the given amount of cycles
    fn advance_cycles(&mut self, ticks: Ticks) -> Result<()> {
        for _ in 0..ticks {
            self.cycles += 1;
            self.bus.tick(1)?;
        }
        Ok(())
    }

    /// Reads a value from the bus and spends ticks.
    fn read_ticks<T: CpuSized>(&mut self, addr: Address) -> Result<T> {
        let mut result: T = T::zero();

        // Below converts from BE -> LE on the fly
        for a in 0..std::mem::size_of::<T>() {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b: T = self.bus.read(byte_addr).into();
            result = result.wrapping_shl(8) | b;

            self.advance_cycles(2)?;
        }

        if std::mem::size_of::<T>() == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(result)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    fn write_ticks<T: CpuSized>(&mut self, addr: Address, value: T) -> Result<()> {
        let mut val: Long = value.to_be().into();

        for a in 0..std::mem::size_of::<T>() {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b = val as u8;
            val = val >> 8;

            self.bus.write(byte_addr, b);
            self.advance_cycles(2)?;
        }

        if std::mem::size_of::<T>() == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// High-to-low temporal order.
    fn write_ticks_th<T: CpuSized>(&mut self, addr: Address, value: T) -> Result<()> {
        let mut val: Long = value.into();

        for a in (0..std::mem::size_of::<T>()).rev() {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b = val as u8;
            val = val >> 8;

            self.bus.write(byte_addr, b);
            self.advance_cycles(2)?;
        }

        if std::mem::size_of::<T>() == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// Pushes 16-bit to supervisor stack
    fn push16_ss(&mut self, val: u16) -> Result<()> {
        self.regs.ssp = self.regs.ssp.wrapping_sub(2);
        self.write_ticks(self.regs.ssp, val)
    }

    /// Pushes 32-bit to supervisor stack
    fn push32_ss(&mut self, val: u32) -> Result<()> {
        self.regs.ssp = self.regs.ssp.wrapping_sub(4);
        self.write_ticks(self.regs.ssp, val)
    }

    /// Sets the program counter and flushes the prefetch queue
    fn set_pc(&mut self, pc: Address) -> Result<()> {
        self.prefetch.clear();
        self.regs.pc = pc.wrapping_sub(4) & ADDRESS_MASK;
        Ok(())
    }

    /// Raises a CPU exception in supervisor mode.
    fn raise_exception(&mut self, vector: Address) -> Result<()> {
        // Advance PC beyond the current instruction to
        // have the right offset for the stack frame.
        // The current prefetch queue length provides an indication
        // of the current instruction length.
        self.regs.pc = self
            .regs
            .pc
            .wrapping_add((2 - (self.prefetch.len() as u32)) * 2);

        self.regs.sr.set_supervisor(true);
        self.push16_ss(self.regs.pc as u16)?;
        self.push32_ss(((self.regs.sr.0 as u32) << 16) | (self.regs.pc >> 16))?;

        let new_pc = self.read_ticks::<Long>(vector)?.into();
        self.set_pc(new_pc)?;
        self.prefetch_pump()?;
        self.advance_cycles(2)?; // 2x idle
        self.prefetch_pump()?;

        Ok(())
    }

    /// Executes a previously decoded instruction.
    fn execute_instruction(&mut self, instr: Instruction) -> Result<()> {
        match instr.mnemonic {
            InstructionMnemonic::AND_l => self.op_bitwise::<Long>(&instr, |a, b| a & b),
            InstructionMnemonic::AND_w => self.op_bitwise::<Word>(&instr, |a, b| a & b),
            InstructionMnemonic::AND_b => self.op_bitwise::<Byte>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_l => self.op_bitwise_immediate::<Long>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_w => self.op_bitwise_immediate::<Word>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_b => self.op_bitwise_immediate::<Byte>(&instr, |a, b| a & b),
            InstructionMnemonic::EOR_l => self.op_bitwise::<Long>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EOR_w => self.op_bitwise::<Word>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EOR_b => self.op_bitwise::<Byte>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_l => self.op_bitwise_immediate::<Long>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_w => self.op_bitwise_immediate::<Word>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_b => self.op_bitwise_immediate::<Byte>(&instr, |a, b| a ^ b),
            InstructionMnemonic::OR_l => self.op_bitwise::<Long>(&instr, |a, b| a | b),
            InstructionMnemonic::OR_w => self.op_bitwise::<Word>(&instr, |a, b| a | b),
            InstructionMnemonic::OR_b => self.op_bitwise::<Byte>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_l => self.op_bitwise_immediate::<Long>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_w => self.op_bitwise_immediate::<Word>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_b => self.op_bitwise_immediate::<Byte>(&instr, |a, b| a | b),
            InstructionMnemonic::NOP => Ok(()),
            InstructionMnemonic::SWAP => self.op_swap(&instr),
            InstructionMnemonic::TRAP => self.op_trap(&instr),
        }
    }

    /// Calculates address from effective addressing mode
    /// Happens once per instruction so e.g. postinc/predec only occur once.
    fn calc_ea_addr<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize) -> Result<Address> {
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
        let addr = match instr.get_addr_mode()? {
            AddressingMode::DataRegister => unreachable!(),
            AddressingMode::Indirect => self.regs.read_a(ea_in),
            AddressingMode::IndirectPreDec => {
                self.advance_cycles(2)?; // 2x idle
                self.regs.read_a_predec(ea_in, std::mem::size_of::<T>())
            }
            AddressingMode::IndirectPostInc => self
                .regs
                .read_a_postinc::<Address>(ea_in, std::mem::size_of::<T>()),
            AddressingMode::IndirectDisplacement => {
                instr.fetch_extwords(|| self.fetch_pump())?;
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = instr.get_displacement()?;
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::IndirectIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extwords(|| self.fetch_pump())?;

                let extword = instr.get_extword(0)?;
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                addr.wrapping_add(displacement).wrapping_add(index)
            }
            AddressingMode::PCDisplacement => {
                instr.fetch_extwords(|| self.fetch_pump())?;
                let addr = self.regs.pc;
                let displacement = instr.get_displacement()?;
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::PCIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extwords(|| self.fetch_pump())?;
                let extword = instr.get_extword(0)?;
                let pc = self.regs.pc;
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                pc.wrapping_add(displacement).wrapping_add(index)
            }
            AddressingMode::AbsoluteShort => self.fetch_pump()? as i16 as i32 as u32,
            AddressingMode::AbsoluteLong => {
                let h = self.fetch_pump()? as u32;
                let l = self.fetch_pump()? as u32;
                (h << 16) | l
            }
            _ => todo!(),
        };

        self.step_ea_addr = Some(addr);
        Ok(addr)
    }

    fn fetch_immediate<T: CpuSized>(&mut self) -> Result<T> {
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

    /// Reads a value from the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn read_ea<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize) -> Result<T> {
        let v = match instr.get_addr_mode()? {
            AddressingMode::DataRegister => self.regs.read_d(ea_in),
            AddressingMode::Immediate => self.fetch_immediate::<T>()?,
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectPreDec
            | AddressingMode::IndirectPostInc
            | AddressingMode::PCDisplacement
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr::<T>(instr, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectIndex | AddressingMode::PCIndex => {
                let addr = self.calc_ea_addr::<T>(instr, ea_in)?;
                self.read_ticks(addr)?
            }
            _ => todo!(),
        };

        Ok(v)
    }

    /// Writes a value to the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn write_ea<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize, value: T) -> Result<()> {
        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => Ok(self.regs.write_d(ea_in, value)),
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectIndex
            | AddressingMode::IndirectPreDec
            | AddressingMode::IndirectPostInc
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr::<T>(instr, ea_in)?;
                self.write_ticks_th(addr, value)
            }
            _ => todo!(),
        }
    }

    /// SWAP
    pub fn op_swap(&mut self, instr: &Instruction) -> Result<()> {
        let v: Long = self.regs.read_d(instr.get_op2());
        let result = (v >> 16) | (v << 16);

        self.regs.write_d(instr.get_op2(), result);
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & (1 << 31) != 0);
        self.regs.sr.set_z(result == 0);

        Ok(())
    }

    /// AND/OR
    pub fn op_bitwise<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T) -> T,
    ) -> Result<()> {
        let left: T = self.regs.read_d(instr.get_op1());
        let right: T = self.read_ea(instr, instr.get_op2())?;
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let result = calcfn(a, b);

        self.prefetch_pump()?;
        match instr.get_direction() {
            Direction::Right => self.regs.write_d(instr.get_op1(), result),
            Direction::Left => self.write_ea(instr, instr.get_op2(), result)?,
        }
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs
            .sr
            .set_n(result.reverse_bits() & T::one() != T::zero());
        self.regs.sr.set_z(result == T::zero());

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => {
                self.advance_cycles(4)?
            }

            (_, Direction::Right, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// ANDI/ORI
    pub fn op_bitwise_immediate<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T) -> T,
    ) -> Result<()> {
        let a: T = self.fetch_immediate()?;
        let b: T = self.read_ea(instr, instr.get_op2())?;
        let result = calcfn(a, b);

        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs
            .sr
            .set_n(result.reverse_bits() & T::one() != T::zero());
        self.regs.sr.set_z(result == T::zero());

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(4)?,
            _ => (),
        };

        Ok(())
    }

    /// TRAP
    pub fn op_trap(&mut self, instr: &Instruction) -> Result<()> {
        self.advance_cycles(4)?; // idle
        self.raise_exception(instr.trap_get_vector() * 4 + 0x80)?;
        Ok(())
    }
}

impl<TBus> Tickable for CpuM68k<TBus>
where
    TBus: Bus<Address>,
{
    fn tick(&mut self, _ticks: Ticks) -> Result<Ticks> {
        self.step()?;

        Ok(0)
    }
}
