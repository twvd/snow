use anyhow::{anyhow, Result};

/// Instruction mnemonic
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InstructionMnemonic {
    NOP,
}

/// A decoded instruction
pub struct Instruction {
    pub mnemonic: InstructionMnemonic,
}

impl Instruction {
    #[rustfmt::skip]
    const DECODE_TABLE: &'static [(u16, u16, InstructionMnemonic)] = &[(
        0b0100_1110_0111_0001, 0b1111_1111_1111_1111, InstructionMnemonic::NOP,
    )];

    /// Attempts to decode an instruction from a 16-bit input
    pub fn try_decode(data: u16) -> Result<Instruction> {
        for &(val, mask, mnemonic) in Self::DECODE_TABLE.into_iter() {
            if data & mask == val {
                return Ok(Instruction { mnemonic });
            }
        }

        Err(anyhow!("Cannot decode instruction: {:016b}", data))
    }
}
