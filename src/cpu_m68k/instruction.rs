use anyhow::{anyhow, Result};

/// Instruction mnemonic
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InstructionMnemonic {
    NOP,
    SWAP,
}

/// Addressing modes
#[derive(Debug, Eq, PartialEq)]
pub enum AddressingMode {
    DataRegister,
}

/// A decoded instruction
pub struct Instruction {
    pub mnemonic: InstructionMnemonic,
    pub data: u16,
}

impl Instruction {
    #[rustfmt::skip]
    const DECODE_TABLE: &'static [(u16, u16, InstructionMnemonic)] = &[
        (0b0100_1110_0111_0001, 0b1111_1111_1111_1111, InstructionMnemonic::NOP),
        (0b0100_1000_0100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::SWAP),
    ];

    /// Attempts to decode an instruction from a 16-bit input
    pub fn try_decode(data: u16) -> Result<Instruction> {
        for &(val, mask, mnemonic) in Self::DECODE_TABLE.into_iter() {
            if data & mask == val {
                return Ok(Instruction { mnemonic, data });
            }
        }

        Err(anyhow!("Cannot decode instruction: {:016b}", data))
    }

    /// Gets the addressing mode of this instruction
    pub fn get_addr_mode(&self) -> Result<AddressingMode> {
        match self.data & 0b111_000 {
            0b000_000 => Ok(AddressingMode::DataRegister),
            _ => Err(anyhow!("Invalid addressing mode {:06b}", self.data & 0b111_111)),
        }
    }

    pub fn get_dr(&self) -> usize {
        debug_assert_eq!(self.get_addr_mode().unwrap(), AddressingMode::DataRegister);
        usize::from(self.data & 0b111)
    }
}
