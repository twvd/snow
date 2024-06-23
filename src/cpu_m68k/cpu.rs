use std::collections::VecDeque;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};

use super::instruction::{
    AddressingMode, Direction, IndexSize, Instruction, InstructionMnemonic, Xn,
};
use super::regs::RegisterFile;

/// Motorola 680x0
#[derive(Serialize, Deserialize)]
pub struct CpuM68k<TBus: Bus<Address>> {
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
    pub prefetch: VecDeque<u16>,
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
        }
    }

    fn prefetch_pump(&mut self) -> Result<()> {
        let fetch_addr = ((self.regs.pc & !1) + 4) & ADDRESS_MASK;
        let new_item = self.read16_ticks(fetch_addr)?;
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
    fn fetch(&mut self) -> Result<u16> {
        if self.prefetch.len() == 0 {
            self.prefetch_pump()?;
        }
        Ok(self.prefetch.pop_front().unwrap())
    }

    /// Executes a single CPU step.
    pub fn step(&mut self) -> Result<()> {
        debug_assert_eq!(self.prefetch.len(), 2);

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

    /// Reads a 16-bit value from the bus and spends ticks.
    fn read16_ticks(&mut self, addr: Address) -> Result<u16> {
        let v = self.bus.read16(addr);
        self.advance_cycles(4)?;
        Ok(v)
    }

    /// Writes a 16-bit value to the bus and spends ticks.
    fn write16_ticks(&mut self, addr: Address, value: u16) -> Result<()> {
        self.bus.write16(addr, value);
        self.advance_cycles(4)?;
        Ok(())
    }

    /// Reads a 32-bit value from the bus and spends ticks.
    fn read32_ticks(&mut self, addr: Address) -> Result<u32> {
        let h = self.read16_ticks(addr)? as u32;
        let l = self.read16_ticks(addr.wrapping_add(2))? as u32;
        Ok((h << 16) | l)
    }

    /// Writes a 32-bit value to the bus and spends ticks.
    fn write32_ticks(&mut self, addr: Address, value: u32) -> Result<()> {
        self.write16_ticks(addr, (value >> 16) as u16)?;
        self.write16_ticks(addr.wrapping_add(2), value as u16)?;
        Ok(())
    }

    /// Pushes 16-bit to supervisor stack
    fn push16_ss(&mut self, val: u16) -> Result<()> {
        self.regs.ssp = self.regs.ssp.wrapping_sub(2);
        self.write16_ticks(self.regs.ssp, val)
    }

    /// Pushes 32-bit to supervisor stack
    fn push32_ss(&mut self, val: u32) -> Result<()> {
        self.regs.ssp = self.regs.ssp.wrapping_sub(4);
        self.write32_ticks(self.regs.ssp, val)
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

        let new_pc = self.read32_ticks(vector)?.into();
        self.set_pc(new_pc)?;
        self.prefetch_pump()?;
        self.advance_cycles(2)?; // 2x idle
        self.prefetch_pump()?;

        Ok(())
    }

    /// Executes a previously decoded instruction.
    pub fn execute_instruction(&mut self, instr: Instruction) -> Result<()> {
        match instr.mnemonic {
            InstructionMnemonic::AND => self.op_and(&instr),
            InstructionMnemonic::NOP => Ok(()),
            InstructionMnemonic::SWAP => self.op_swap(&instr),
            InstructionMnemonic::TRAP => self.op_trap(&instr),
        }
    }

    pub fn read_ea(&mut self, instr: &Instruction, ea_in: usize) -> Result<u32> {
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

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => Ok(self.regs.read_d(ea_in)),
            AddressingMode::IndirectDisplacement => {
                let addr = self.regs.read_a(ea_in);
                let displacement = instr.get_displacement()?;
                let op_ptr = Address::from(addr.wrapping_add_signed(displacement));
                let operand = self.read32_ticks(op_ptr)?;
                Ok(operand)
            }
            AddressingMode::IndirectIndex => {
                // 2 idle cycles
                self.advance_cycles(2)?;
                self.prefetch_refill()?;

                let extword = instr.get_extword(0)?;
                let addr = self.regs.read_a(ea_in);
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                let op_ptr = addr.wrapping_add(displacement).wrapping_add(index);
                let operand = self.read32_ticks(op_ptr)?;
                // 2 idle cycles
                self.advance_cycles(2)?;
                Ok(operand)
            }
            AddressingMode::PCIndex => {
                todo!();
                self.prefetch_refill()?;
                let extword = instr.get_extword(0)?;
                let pc = self
                    .regs
                    .pc
                    .wrapping_add((2 - (self.prefetch.len() as u32)) * 2);
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                let op_ptr = pc.wrapping_add(displacement).wrapping_add(index);
                let operand = self.read32_ticks(op_ptr)?;
                Ok(operand)
            }
            AddressingMode::AbsoluteShort => self.read32_ticks(instr.get_absolute()?),
            AddressingMode::AbsoluteLong => self.read32_ticks(instr.get_absolute()?),
            _ => todo!(),
        }
    }

    pub fn write_ea(&mut self, instr: &Instruction, ea_in: usize, value: u32) -> Result<()> {
        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => Ok(self.regs.write_d(ea_in, value)),
            AddressingMode::IndirectDisplacement => {
                let addr = self.regs.read_a(ea_in);
                let displacement = instr.get_displacement()?;
                let op_ptr = Address::from(addr.wrapping_add_signed(displacement));
                self.write32_ticks(op_ptr, value)?;
                Ok(())
            }
            _ => todo!(),
        }
    }

    /// SWAP
    pub fn op_swap(&mut self, instr: &Instruction) -> Result<()> {
        let v = self.regs.read_d(instr.get_op2());
        let result = (v >> 16) | (v << 16);

        self.regs.write_d(instr.get_op2(), result);
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & (1 << 31) != 0);
        self.regs.sr.set_z(result == 0);

        Ok(())
    }

    /// AND
    pub fn op_and(&mut self, instr: &Instruction) -> Result<()> {
        let left = self.regs.read_d(instr.get_op1());
        let right = self.read_ea(instr, instr.get_op2())?;
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let result = a & b;

        match instr.get_direction() {
            Direction::Right => self.regs.write_d(instr.get_op1(), result),
            Direction::Left => self.write_ea(instr, instr.get_op2(), result)?,
        }
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & (1 << 31) != 0);
        self.regs.sr.set_z(result == 0);

        // Idle cycles
        match instr.get_addr_mode()? {
            AddressingMode::AbsoluteShort | AddressingMode::AbsoluteLong => {
                self.advance_cycles(2)?
            }
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
