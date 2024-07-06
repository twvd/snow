use anyhow::{anyhow, Context, Result};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

use super::{CpuSized, Long, Word};

use crate::bus::Address;

use std::cell::Cell;

/// Instruction mnemonic
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InstructionMnemonic {
    ABCD,
    SBCD,
    ADD_l,
    ADD_w,
    ADD_b,
    ADDA_l,
    ADDA_w,
    // no ADDA_b
    ADDI_l,
    ADDI_w,
    ADDI_b,
    ADDQ_l,
    ADDQ_w,
    ADDQ_b,
    ADDX_l,
    ADDX_w,
    ADDX_b,
    AND_l,
    AND_w,
    AND_b,
    ANDI_l,
    ANDI_w,
    ANDI_b,
    ANDI_ccr,
    ANDI_sr,
    BCHG_dn,
    BCLR_dn,
    BSET_dn,
    BTST_dn,
    BCHG_imm,
    BCLR_imm,
    BSET_imm,
    BTST_imm,
    CLR_l,
    CLR_w,
    CLR_b,
    CMP_l,
    CMP_w,
    CMP_b,
    CMPA_l,
    CMPA_w,
    // no CMPA_b
    CMPI_l,
    CMPI_w,
    CMPI_b,
    CMPM_l,
    CMPM_w,
    CMPM_b,
    // no DIVS_l, DIVS_b
    DIVS_w,
    // no DIVU_l, DIVU_b
    DIVU_w,
    EOR_l,
    EOR_w,
    EOR_b,
    EORI_l,
    EORI_w,
    EORI_b,
    EORI_ccr,
    EORI_sr,
    OR_l,
    OR_w,
    OR_b,
    ORI_l,
    ORI_w,
    ORI_b,
    ORI_ccr,
    ORI_sr,
    NOP,
    MOVE_w,
    MOVE_l,
    MOVE_b,
    MOVEA_w,
    MOVEA_l,
    MOVEP_w,
    MOVEP_l,
    MOVEfromSR,
    MOVEfromUSP,
    MOVEtoCCR,
    MOVEtoSR,
    MOVEtoUSP,
    // no MULU_l, MULU_b
    MULU_w,
    // no MULS_l, MULS_b
    MULS_w,
    NEG_l,
    NEG_w,
    NEG_b,
    NEGX_l,
    NEGX_w,
    NEGX_b,
    NOT_l,
    NOT_w,
    NOT_b,
    SUB_l,
    SUB_w,
    SUB_b,
    SUBA_l,
    SUBA_w,
    // no SUBA_b
    SUBI_l,
    SUBI_w,
    SUBI_b,
    SUBQ_l,
    SUBQ_w,
    SUBQ_b,
    SUBX_l,
    SUBX_w,
    SUBX_b,
    SWAP,
    TRAP,
}

/// Addressing modes
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
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
    Immediate,
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

/// A decoded instruction
#[derive(Debug)]
pub struct Instruction {
    pub mnemonic: InstructionMnemonic,
    pub data: u16,
    pub extword: Cell<Option<ExtWord>>,
}

impl Instruction {
    #[rustfmt::skip]
    const DECODE_TABLE: &'static [(u16, u16, InstructionMnemonic)] = &[
        (0b0000_0000_0011_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ORI_ccr),
        (0b0000_0000_0111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ORI_sr),
        (0b0000_0000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ORI_b),
        (0b0000_0000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ORI_w),
        (0b0000_0000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ORI_l),
        (0b0000_0010_0011_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ANDI_ccr),
        (0b0000_0010_0111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ANDI_sr),
        (0b0000_0010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ANDI_b),
        (0b0000_0010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ANDI_w),
        (0b0000_0010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ANDI_l),
        (0b0000_0100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::SUBI_b),
        (0b0000_0100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::SUBI_w),
        (0b0000_0100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::SUBI_l),
        (0b0000_0110_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ADDI_b),
        (0b0000_0110_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ADDI_w),
        (0b0000_0110_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ADDI_l),
        (0b0000_1010_0011_1100, 0b1111_1111_1111_1111, InstructionMnemonic::EORI_ccr),
        (0b0000_1010_0111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::EORI_sr),
        (0b0000_1010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::EORI_b),
        (0b0000_1010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::EORI_w),
        (0b0000_1010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::EORI_l),
        (0b0000_1100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CMPI_b),
        (0b0000_1100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CMPI_w),
        (0b0000_1100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CMPI_l),
        (0b0011_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MOVEA_w),
        (0b0010_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MOVEA_l),
        (0b0001_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::MOVE_b),
        (0b0011_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::MOVE_w),
        (0b0010_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::MOVE_l),
        (0b0000_0001_0000_1000, 0b1111_0001_0111_1000, InstructionMnemonic::MOVEP_w),
        (0b0000_0001_0100_1000, 0b1111_0001_0111_1000, InstructionMnemonic::MOVEP_l),
        (0b0100_0000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEfromSR),
        (0b0100_0100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEtoCCR),
        (0b0100_0110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEtoSR),
        (0b0100_1110_0110_1000, 0b1111_1111_1111_1000, InstructionMnemonic::MOVEfromUSP),
        (0b0100_1110_0110_0000, 0b1111_1111_1111_1000, InstructionMnemonic::MOVEtoUSP),
        (0b0100_0100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEG_b),
        (0b0100_0100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEG_w),
        (0b0100_0100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEG_l),
        (0b0100_0000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEGX_b),
        (0b0100_0000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEGX_w),
        (0b0100_0000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEGX_l),
        (0b0100_0010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CLR_b),
        (0b0100_0010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CLR_w),
        (0b0100_0010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CLR_l),
        (0b0100_0110_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NOT_b),
        (0b0100_0110_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NOT_w),
        (0b0100_0110_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NOT_l),
        (0b0000_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BTST_dn),
        (0b0000_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BCHG_dn),
        (0b0000_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BCLR_dn),
        (0b0000_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BSET_dn),
        (0b0000_1000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BTST_imm),
        (0b0000_1000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BCHG_imm),
        (0b0000_1000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BCLR_imm),
        (0b0000_1000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BSET_imm),
        (0b0100_1000_0100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::SWAP),
        (0b0100_1110_0100_0000, 0b1111_1111_1111_0000, InstructionMnemonic::TRAP),
        (0b0100_1110_0111_0001, 0b1111_1111_1111_1111, InstructionMnemonic::NOP),
        (0b1000_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::OR_b),
        (0b1000_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::OR_w),
        (0b1000_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::OR_l),
        (0b1000_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::DIVU_w),
        (0b1000_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::DIVS_w),
        (0b0101_0000_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDQ_b),
        (0b0101_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDQ_w),
        (0b0101_0000_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDQ_l),
        (0b0101_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBQ_b),
        (0b0101_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBQ_w),
        (0b0101_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBQ_l),
        (0b1001_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SUBX_b),
        (0b1001_0001_0100_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SUBX_w),
        (0b1001_0001_1000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SUBX_l),
        (0b1001_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::SUB_b),
        (0b1001_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::SUB_w),
        (0b1001_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::SUB_l),
        (0b1001_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBA_w),
        (0b1001_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBA_l),
        (0b1011_0001_0000_1000, 0b1111_0001_1111_1000, InstructionMnemonic::CMPM_b),
        (0b1011_0001_0100_1000, 0b1111_0001_1111_1000, InstructionMnemonic::CMPM_w),
        (0b1011_0001_1000_1000, 0b1111_0001_1111_1000, InstructionMnemonic::CMPM_l),
        (0b1011_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::EOR_b),
        (0b1011_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::EOR_w),
        (0b1011_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::EOR_l),
        (0b1011_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMPA_w),
        (0b1011_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMPA_l),
        (0b1011_0000_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMP_b),
        (0b1011_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMP_w),
        (0b1011_0000_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMP_l),
        (0b1100_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MULU_w),
        (0b1100_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MULS_w),
        (0b1100_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::AND_b),
        (0b1100_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::AND_w),
        (0b1100_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::AND_l),
        (0b1101_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ADDX_b),
        (0b1101_0001_0100_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ADDX_w),
        (0b1101_0001_1000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ADDX_l),
        (0b1101_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::ADD_b),
        (0b1101_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::ADD_w),
        (0b1101_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::ADD_l),
        (0b1101_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDA_w),
        (0b1101_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDA_l),
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
                    extword: Cell::new(None),
                });
            }
        }

        Err(anyhow!("Cannot decode instruction: {:016b}", data))
    }

    /// Gets the addressing mode of this instruction
    pub fn get_addr_mode(&self) -> Result<AddressingMode> {
        debug_assert!(match self.mnemonic {
            InstructionMnemonic::ADDX_l
            | InstructionMnemonic::ADDX_w
            | InstructionMnemonic::ADDX_b
            | InstructionMnemonic::SUBX_l
            | InstructionMnemonic::SUBX_w
            | InstructionMnemonic::SUBX_b
            | InstructionMnemonic::ABCD
            | InstructionMnemonic::SBCD => false,
            _ => true,
        });

        Self::decode_addr_mode((self.data & 0b111_000) >> 3, self.data & 0b000_111)
    }

    /// Gets the addressing mode on the 'left' side of this instruction
    /// (for MOVE)
    pub fn get_addr_mode_left(&self) -> Result<AddressingMode> {
        debug_assert!(match self.mnemonic {
            InstructionMnemonic::MOVE_l
            | InstructionMnemonic::MOVE_w
            | InstructionMnemonic::MOVE_b => true,
            _ => false,
        });

        Self::decode_addr_mode(
            (self.data & 0b111_000_000) >> 6,
            (self.data & 0b111_000_000_000) >> 9,
        )
    }

    /// Decodes an addressing mode from the mode and Xn fields of an instruction.
    fn decode_addr_mode(mode: Word, xn: Word) -> Result<AddressingMode> {
        match (mode, xn) {
            (0b000, _) => Ok(AddressingMode::DataRegister),
            (0b001, _) => Ok(AddressingMode::AddressRegister),
            (0b010, _) => Ok(AddressingMode::Indirect),
            (0b011, _) => Ok(AddressingMode::IndirectPostInc),
            (0b100, _) => Ok(AddressingMode::IndirectPreDec),
            (0b101, _) => Ok(AddressingMode::IndirectDisplacement),
            (0b110, _) => Ok(AddressingMode::IndirectIndex),
            (0b111, 0b000) => Ok(AddressingMode::AbsoluteShort),
            (0b111, 0b001) => Ok(AddressingMode::AbsoluteLong),
            (0b111, 0b010) => Ok(AddressingMode::PCDisplacement),
            (0b111, 0b011) => Ok(AddressingMode::PCIndex),
            (0b111, 0b100) => Ok(AddressingMode::Immediate),
            _ => Err(anyhow!("Invalid addressing mode {:03b} {:03b}", mode, xn)),
        }
    }

    /// Gets the addressing mode of this instruction, for xxxX, xBCD instructions
    pub fn get_addr_mode_x(&self) -> Result<AddressingMode> {
        if self.data & 0b1000 != 0 {
            Ok(AddressingMode::IndirectPreDec)
        } else {
            Ok(AddressingMode::DataRegister)
        }
    }

    /// Gets operation direction, for MOVEP
    pub fn get_direction_movep(&self) -> Direction {
        debug_assert!(
            self.mnemonic == InstructionMnemonic::MOVEP_l
                || self.mnemonic == InstructionMnemonic::MOVEP_w
        );
        Direction::from_u16((!self.data >> 7) & 1).unwrap()
    }

    /// Gets operation direction
    pub fn get_direction(&self) -> Direction {
        debug_assert!(
            self.mnemonic != InstructionMnemonic::MOVEP_l
                && self.mnemonic != InstructionMnemonic::MOVEP_w
        );
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

    pub fn fetch_extword<F>(&self, mut fetch: F) -> Result<()>
    where
        F: FnMut() -> Result<u16>,
    {
        if !self.has_extword() {
            // This check is to handle 'MOVE mem, (xxx).L' properly.
            self.extword.set(Some(fetch()?.into()));
        }
        Ok(())
    }

    pub fn clear_extword(&self) {
        self.extword.set(None)
    }

    pub fn has_extword(&self) -> bool {
        self.extword.get().is_some()
    }

    pub fn get_extword(&self) -> Result<ExtWord> {
        self.extword.get().context("Ext word not fetched")
    }

    pub fn get_displacement(&self) -> Result<i32> {
        debug_assert!(
            (((self.mnemonic == InstructionMnemonic::MOVE_b)
                || (self.mnemonic == InstructionMnemonic::MOVE_l)
                || (self.mnemonic == InstructionMnemonic::MOVE_w))
                && (self.get_addr_mode_left().unwrap() == AddressingMode::IndirectDisplacement
                    || self.get_addr_mode_left().unwrap() == AddressingMode::PCDisplacement))
                || self.get_addr_mode().unwrap() == AddressingMode::IndirectDisplacement
                || self.get_addr_mode().unwrap() == AddressingMode::PCDisplacement
                || self.mnemonic == InstructionMnemonic::MOVEP_l
                || self.mnemonic == InstructionMnemonic::MOVEP_w
        );
        debug_assert!(self.extword.get().is_some());

        Ok(self.get_extword()?.into())
    }

    /// Retrieves the data part of 'quick' instructions
    pub fn get_quick<T: CpuSized>(&self) -> T {
        let result = T::chop(((self.data as Long) >> 9) & 0b111);
        if result == T::zero() {
            8.into()
        } else {
            result
        }
    }
}
