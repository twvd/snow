use anyhow::{anyhow, Context, Result};
use arrayvec::ArrayVec;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

use crate::bus::Address;

use std::cell::RefCell;

/// Instruction mnemonic
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InstructionMnemonic {
    AND,
    NOP,
    SWAP,
    TRAP,
}

/// Addressing modes
#[derive(Debug, Eq, PartialEq)]
pub enum AddressingMode {
    DataRegister,
    AddressRegister,
    Indirect,
    IndirectPostInc,
    IndirectPreDec,
    IndirectDisplacement,
    IndirectIndex,
    PCDisplacement,
    PCIndex,
    AbsoluteShort,
    AbsoluteLong,
}

/// Direction
#[derive(FromPrimitive, Debug, Eq, PartialEq)]
pub enum Direction {
    Right = 0,
    Left = 1,
}

/// Extension word
#[derive(Debug, Clone, Copy)]
pub struct ExtWord {
    pub data: u16,
}

/// Register type (for D/A)
#[derive(FromPrimitive, Debug, Eq, PartialEq)]
pub enum Xn {
    Dn = 0,
    An = 1,
}

/// Index size (for W/L)
#[derive(FromPrimitive, Debug, Eq, PartialEq)]
pub enum IndexSize {
    /// Sign extended word
    Word = 0,
    /// Long
    Long = 1,
}

impl From<u16> for ExtWord {
    fn from(data: u16) -> Self {
        Self { data }
    }
}

impl Into<u16> for ExtWord {
    fn into(self) -> u16 {
        self.data
    }
}

impl Into<u32> for ExtWord {
    fn into(self) -> u32 {
        self.data as u32
    }
}

impl Into<i32> for ExtWord {
    fn into(self) -> i32 {
        self.data as i16 as i32
    }
}

impl ExtWord {
    pub fn to_address(&self) -> Address {
        self.data as Address
    }

    pub fn to_address_signext(&self) -> Address {
        self.data as i16 as i32 as Address
    }

    pub fn brief_get_displacement(&self) -> Address {
        Address::from(self.data & 0xFF)
    }

    pub fn brief_get_displacement_signext(&self) -> u32 {
        self.data as u8 as i8 as i32 as u32
    }

    pub fn brief_get_register(&self) -> (Xn, usize) {
        (
            Xn::from_u16(self.data >> 15).unwrap(),
            usize::from((self.data >> 12) & 0b111),
        )
    }

    pub fn brief_get_index_size(&self) -> IndexSize {
        IndexSize::from_u16((self.data >> 11) & 1).unwrap()
    }
}

type ExtWords = ArrayVec<ExtWord, 4>;

/// A decoded instruction
pub struct Instruction {
    pub mnemonic: InstructionMnemonic,
    pub data: u16,
    pub extwords: RefCell<Option<ExtWords>>,
}

impl Instruction {
    #[rustfmt::skip]
    const DECODE_TABLE: &'static [(u16, u16, InstructionMnemonic)] = &[
        (0b1100_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::AND),
        (0b0100_1110_0111_0001, 0b1111_1111_1111_1111, InstructionMnemonic::NOP),
        (0b0100_1000_0100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::SWAP),
        (0b0100_1110_0100_0000, 0b1111_1111_1111_0000, InstructionMnemonic::TRAP),
    ];

    /// Attempts to decode an instruction from a fetch input function.
    pub fn try_decode<F>(mut fetch: F) -> Result<Instruction>
    where
        F: FnMut() -> Result<u16>,
    {
        let data = fetch()?;
        for &(val, mask, mnemonic) in Self::DECODE_TABLE.into_iter() {
            if data & mask == val {
                return Ok(Instruction {
                    mnemonic,
                    data,
                    extwords: RefCell::new(None),
                });
            }
        }

        Err(anyhow!("Cannot decode instruction: {:016b}", data))
    }

    /// Gets the addressing mode of this instruction
    pub fn get_addr_mode(&self) -> Result<AddressingMode> {
        match ((self.data & 0b111_000) >> 3, self.data & 0b000_111) {
            (0b000, _) => Ok(AddressingMode::DataRegister),
            (0b010, _) => Ok(AddressingMode::Indirect),
            (0b100, _) => Ok(AddressingMode::IndirectPreDec),
            (0b101, _) => Ok(AddressingMode::IndirectDisplacement),
            (0b110, _) => Ok(AddressingMode::IndirectIndex),
            (0b111, 0b001) => Ok(AddressingMode::AbsoluteLong),
            (0b111, 0b011) => Ok(AddressingMode::PCIndex),
            _ => Err(anyhow!(
                "Invalid addressing mode {:06b}",
                self.data & 0b111_111
            )),
        }
    }

    pub fn get_direction(&self) -> Direction {
        Direction::from_u16((self.data >> 8) & 1).unwrap()
    }

    pub fn get_op1(&self) -> usize {
        usize::from((self.data & 0b0000_1110_0000_0000) >> 9)
    }

    pub fn get_op2(&self) -> usize {
        usize::from(self.data & 0b111)
    }

    pub fn trap_get_vector(&self) -> u32 {
        debug_assert_eq!(self.mnemonic, InstructionMnemonic::TRAP);

        u32::from(self.data & 0b1111)
    }

    pub fn get_num_extwords(&self) -> Result<usize> {
        match self.get_addr_mode()? {
            AddressingMode::IndirectDisplacement => Ok(1),
            AddressingMode::IndirectIndex => Ok(1),
            AddressingMode::PCIndex => Ok(1),
            _ => Ok(0),
        }
    }

    pub fn fetch_extwords<F>(&self, mut fetch: F) -> Result<()>
    where
        F: FnMut() -> Result<u16>,
    {
        if self.get_num_extwords()? == 0 {
            return Ok(());
        }

        let mut extwords = ExtWords::new();
        for _ in 0..self.get_num_extwords()? {
            extwords.push(fetch()?.into());
        }
        *self.extwords.borrow_mut() = Some(extwords);
        Ok(())
    }

    pub fn get_extword(&self, idx: usize) -> Result<ExtWord> {
        Ok(*self
            .extwords
            .borrow()
            .as_ref()
            .unwrap() // assuming ext words were fetched
            .get(idx)
            .context("Extension word missing")?)
    }

    pub fn get_displacement(&self) -> Result<i32> {
        debug_assert_eq!(
            self.get_addr_mode().unwrap(),
            AddressingMode::IndirectDisplacement
        );
        debug_assert!(self.extwords.borrow().is_some());
        debug_assert_eq!(self.extwords.borrow().as_ref().unwrap().len(), 1);

        Ok(self.get_extword(0)?.into())
    }
}
