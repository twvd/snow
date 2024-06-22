use std::collections::VecDeque;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};

use super::instruction::{AddressingMode, Direction, Instruction, InstructionMnemonic, Xn};
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

    /// Re-fills the prefetch queue
    fn prefetch_refill(&mut self) -> Result<()> {
        while self.prefetch.len() < 2 {
            let fetch_addr = ((self.regs.pc & !1) + 4) & ADDRESS_MASK;
            let new_item = self.read16_ticks(fetch_addr, 4)?;
            self.prefetch.push_back(new_item);
            self.regs.pc = (self.regs.pc + 2) & ADDRESS_MASK;
        }
        Ok(())
    }

    /// Fetches a 16-bit value, through the prefetch queue
    fn fetch(&mut self) -> Result<u16> {
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
    fn read16_ticks(&mut self, addr: Address, ticks: Ticks) -> Result<u16> {
        self.advance_cycles(ticks)?;
        Ok(self.bus.read16(addr))
    }

    /// Writes a 16-bit value to the bus and spends ticks.
    fn write16_ticks(&mut self, addr: Address, value: u16, ticks: Ticks) -> Result<()> {
        self.advance_cycles(ticks)?;
        Ok(self.bus.write16(addr, value))
    }

    /// Reads a 32-bit value from the bus and spends ticks.
    fn read32_ticks(&mut self, addr: Address, ticks: Ticks) -> Result<u32> {
        if addr & 1 != 0 {
            panic!("Unaligned access");
        }
        self.advance_cycles(ticks)?;
        Ok(self.bus.read32(addr))
    }

    /// Writes a 32-bit value to the bus and spends ticks.
    fn write32_ticks(&mut self, addr: Address, value: u32, ticks: Ticks) -> Result<()> {
        self.advance_cycles(ticks)?;
        Ok(self.bus.write32(addr, value))
    }

    /// Pushes 16-bit to supervisor stack
    fn push16_ss(&mut self, val: u16) -> Result<()> {
        self.regs.ssp = self.regs.ssp.wrapping_sub(2);
        self.write16_ticks(self.regs.ssp, val, 4)
    }

    /// Pushes 32-bit to supervisor stack
    fn push32_ss(&mut self, val: u32) -> Result<()> {
        self.regs.ssp = self.regs.ssp.wrapping_sub(4);
        self.write32_ticks(self.regs.ssp, val, 8)
    }

    /// Sets the program counter and flushes the prefetch queue
    fn set_pc(&mut self, pc: Address) -> Result<()> {
        self.prefetch.clear();
        self.regs.pc = pc.wrapping_sub(4) & ADDRESS_MASK;
        self.prefetch_refill()?;
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
        self.push32_ss(self.regs.pc)?;
        self.push16_ss(self.regs.sr.0)?;

        let new_pc = self.read32_ticks(vector, 8)?.into();
        self.set_pc(new_pc)
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
        let read_idx = |(xn, reg)| match xn {
            Xn::Dn => self.regs.d[reg],
            Xn::An => self.regs.a[reg],
        };

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => Ok(self.regs.d[ea_in]),
            AddressingMode::IndirectDisplacement => {
                let addr = self.regs.a[ea_in];
                let displacement = instr.get_displacement()?;
                let op_ptr = Address::from(addr.wrapping_add_signed(displacement));
                let operand = self.read32_ticks(op_ptr, 8)?;
                Ok(operand)
            }
            AddressingMode::PCIndex => {
                let pc = self.regs.pc;
                let displacement = instr.get_extword(0)?.brief_get_displacement_signext();
                let index = read_idx(instr.get_extword(0)?.brief_get_register());
                let scale = instr.get_extword(0)?.brief_get_scale();
                let op_ptr = pc
                    .wrapping_add_signed(displacement)
                    .wrapping_add(index.wrapping_mul(scale));
                let operand = self.read32_ticks(op_ptr, 8)?;
                Ok(operand)
            }
            AddressingMode::AbsoluteShort => self.read32_ticks(instr.get_absolute()?, 8),
            AddressingMode::AbsoluteLong => self.read32_ticks(instr.get_absolute()?, 8),
            _ => todo!(),
        }
    }

    pub fn write_ea(&mut self, instr: &Instruction, ea_in: usize, value: u32) -> Result<()> {
        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => Ok(self.regs.d[ea_in] = value),
            AddressingMode::IndirectDisplacement => {
                let addr = self.regs.a[ea_in];
                let displacement = instr.get_displacement()?;
                let op_ptr = Address::from(addr.wrapping_add_signed(displacement));
                self.write32_ticks(op_ptr, value, 8)?;
                Ok(())
            }
            _ => todo!(),
        }
    }

    /// SWAP
    pub fn op_swap(&mut self, instr: &Instruction) -> Result<()> {
        let v = self.regs.d[instr.get_op2()];
        let result = (v >> 16) | (v << 16);

        self.regs.d[instr.get_op2()] = result;
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & (1 << 31) != 0);
        self.regs.sr.set_z(result == 0);

        Ok(())
    }

    /// AND
    pub fn op_and(&mut self, instr: &Instruction) -> Result<()> {
        let left = self.regs.d[instr.get_op1()];
        let right = self.read_ea(instr, instr.get_op2())?;
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let result = a & b;

        match instr.get_direction() {
            Direction::Right => self.regs.d[instr.get_op1()] = result,
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
        self.advance_cycles(4)?;
        self.raise_exception(instr.trap_get_vector() * 4 + 0x80)?;
        self.advance_cycles(2)?;
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
