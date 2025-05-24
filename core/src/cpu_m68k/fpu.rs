use anyhow::Result;

use crate::bus::{Address, Bus, IrqSource};

use super::cpu::CpuM68k;
use super::instruction::Instruction;
use super::CpuM68kType;

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// FNOP
    pub(super) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        // Fetch second word (0000)
        self.fetch()?;

        Ok(())
    }

    /// FSAVE
    pub(super) fn op_fsave(&mut self, instr: &Instruction) -> Result<()> {
        // Idle state frame
        self.write_ea(instr, instr.get_op2(), 0x00180018u32)?;

        Ok(())
    }
}
