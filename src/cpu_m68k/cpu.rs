use std::collections::VecDeque;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};

use super::instruction::{Instruction, InstructionMnemonic};
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

    /// Fetches a 16-bit value, through the prefetch queue
    fn fetch(&mut self) -> u16 {
        debug_assert_eq!(self.prefetch.len(), 2);

        // Re-fill prefetch queue
        let fetch_addr = ((self.regs.pc & !1) + 4) & ADDRESS_MASK;
        let new_item = self.read16_ticks(fetch_addr, 4);
        self.prefetch.push_back(new_item);

        self.regs.pc = (self.regs.pc + 2) & ADDRESS_MASK;
        self.prefetch.pop_front().unwrap()
    }

    /// Executes a single CPU step.
    pub fn step(&mut self) -> Result<()> {
        let raw_instr = self.fetch();
        let instr = Instruction::try_decode(raw_instr)?;
        self.execute_instruction(instr)
    }

    /// Reads a 16-bit value from the bus and spends ticks.
    pub fn read16_ticks(&mut self, addr: Address, ticks: Ticks) -> u16 {
        self.cycles += ticks;
        self.bus.read16(addr)
    }

    /// Executes a previously decoded instruction.
    pub fn execute_instruction(&mut self, instr: Instruction) -> Result<()> {
        match instr.mnemonic {
            InstructionMnemonic::NOP => Ok(()),
        }
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
