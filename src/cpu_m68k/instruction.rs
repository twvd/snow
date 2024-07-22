use anyhow::{anyhow, Context, Result};
use either::Either;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

use super::regs::Register;
use super::{CpuSized, Long, Word};

use crate::bus::Address;

use std::cell::Cell;

/// Instruction mnemonic
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InstructionMnemonic {
    ABCD,
    NBCD,
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
    ASL_ea,
    ASL_b,
    ASL_w,
    ASL_l,
    ASR_b,
    ASR_w,
    ASR_l,
    ASR_ea,
    Bcc,
    BCHG_dn,
    BCLR_dn,
    BSET_dn,
    BTST_dn,
    BCHG_imm,
    BCLR_imm,
    BSET_imm,
    BTST_imm,
    // BRA is actually just Bcc with cond = True
    BSR,
    CHK,
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
    DBcc,
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
    EXG,
    EXT_l,
    EXT_w,
    ILLEGAL,
    JMP,
    JSR,
    LSL_ea,
    LSL_b,
    LSL_w,
    LSL_l,
    LSR_b,
    LSR_w,
    LSR_l,
    LSR_ea,
    OR_l,
    OR_w,
    OR_b,
    ORI_l,
    ORI_w,
    ORI_b,
    ORI_ccr,
    ORI_sr,
    NOP,
    LEA,
    LINK,
    UNLINK,
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
    MOVEM_mem_w,
    MOVEM_mem_l,
    MOVEM_reg_w,
    MOVEM_reg_l,
    MOVEQ,
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
    PEA,
    RESET,
    ROXL_ea,
    ROXL_b,
    ROXL_w,
    ROXL_l,
    ROXR_b,
    ROXR_w,
    ROXR_l,
    ROXR_ea,
    ROL_ea,
    ROL_b,
    ROL_w,
    ROL_l,
    ROR_b,
    ROR_w,
    ROR_l,
    ROR_ea,
    RTE,
    RTR,
    RTS,
    Scc,
    STOP,
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
    TAS,
    TRAP,
    TRAPV,
    TST_l,
    TST_w,
    TST_b,
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
        (0b0100_1000_1000_0000, 0b1111_1111_1111_1000, InstructionMnemonic::EXT_w),
        (0b0100_1000_1100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::EXT_l),
        (0b0100_1000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NBCD),
        (0b0000_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BTST_dn),
        (0b0000_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BCHG_dn),
        (0b0000_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BCLR_dn),
        (0b0000_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BSET_dn),
        (0b0000_1000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BTST_imm),
        (0b0000_1000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BCHG_imm),
        (0b0000_1000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BCLR_imm),
        (0b0000_1000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BSET_imm),
        (0b0100_1000_0100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::SWAP),
        (0b0100_1000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::PEA),
        (0b0100_1010_1111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ILLEGAL),
        (0b0100_1010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TAS),
        (0b0100_1010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TST_b),
        (0b0100_1010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TST_w),
        (0b0100_1010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TST_l),
        (0b0100_1110_0100_0000, 0b1111_1111_1111_0000, InstructionMnemonic::TRAP),
        (0b0100_1110_0101_0000, 0b1111_1111_1111_1000, InstructionMnemonic::LINK),
        (0b0100_1110_0101_1000, 0b1111_1111_1111_1000, InstructionMnemonic::UNLINK),
        (0b0100_1110_0111_0000, 0b1111_1111_1111_1111, InstructionMnemonic::RESET),
        (0b0100_1110_0111_0001, 0b1111_1111_1111_1111, InstructionMnemonic::NOP),
        (0b0100_1110_0111_0010, 0b1111_1111_1111_1111, InstructionMnemonic::STOP),
        (0b0100_1110_0111_0011, 0b1111_1111_1111_1111, InstructionMnemonic::RTE),
        (0b0100_1110_0111_0101, 0b1111_1111_1111_1111, InstructionMnemonic::RTS),
        (0b0100_1110_0111_0110, 0b1111_1111_1111_1111, InstructionMnemonic::TRAPV),
        (0b0100_1110_0111_0111, 0b1111_1111_1111_1111, InstructionMnemonic::RTR),
        (0b0100_1110_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::JSR),
        (0b0100_1110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::JMP),
        (0b0100_1000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_mem_w),
        (0b0100_1000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_mem_l),
        (0b0100_1100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_reg_w),
        (0b0100_1100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_reg_l),
        (0b0100_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::LEA),
        (0b0100_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CHK),
        (0b1000_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SBCD),
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
        (0b0101_0000_1100_1000, 0b1111_0000_1111_1000, InstructionMnemonic::DBcc),
        (0b0101_0000_1100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::Scc),
        // BRA is actually just Bcc with cond = True
        (0b0110_0001_0000_0000, 0b1111_1111_0000_0000, InstructionMnemonic::BSR),
        (0b0110_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::Bcc),
        (0b0111_0000_0000_0000, 0b1111_0001_0000_0000, InstructionMnemonic::MOVEQ),
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
        (0b1100_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ABCD),
        (0b1100_0001_0000_0000, 0b1111_0001_0011_0000, InstructionMnemonic::EXG),
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
        (0b1110_0000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ASR_ea),
        (0b1110_0001_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ASL_ea),
        (0b1110_0010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::LSR_ea),
        (0b1110_0011_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::LSL_ea),
        (0b1110_0100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROXR_ea),
        (0b1110_0101_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROXL_ea),
        (0b1110_0110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROR_ea),
        (0b1110_0111_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROL_ea),
        (0b1110_0000_0000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASR_b),
        (0b1110_0001_0000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASL_b),
        (0b1110_0000_0100_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASR_w),
        (0b1110_0001_0100_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASL_w),
        (0b1110_0000_1000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASR_l),
        (0b1110_0001_1000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASL_l),
        (0b1110_0000_0000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSR_b),
        (0b1110_0001_0000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSL_b),
        (0b1110_0000_0100_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSR_w),
        (0b1110_0001_0100_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSL_w),
        (0b1110_0000_1000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSR_l),
        (0b1110_0001_1000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSL_l),
        (0b1110_0000_0001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXR_b),
        (0b1110_0001_0001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXL_b),
        (0b1110_0000_0101_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXR_w),
        (0b1110_0001_0101_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXL_w),
        (0b1110_0000_1001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXR_l),
        (0b1110_0001_1001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXL_l),
        (0b1110_0000_0001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROR_b),
        (0b1110_0001_0001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROL_b),
        (0b1110_0000_0101_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROR_w),
        (0b1110_0001_0101_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROL_w),
        (0b1110_0000_1001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROR_l),
        (0b1110_0001_1001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROL_l),
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

    pub fn needs_extword(&self) -> bool {
        match self.get_addr_mode().unwrap() {
            AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectIndex
            | AddressingMode::PCDisplacement
            | AddressingMode::PCIndex => true,
            _ => false,
        }
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
                || self.mnemonic == InstructionMnemonic::LINK
                || self.mnemonic == InstructionMnemonic::Bcc
                || self.mnemonic == InstructionMnemonic::DBcc
                || self.mnemonic == InstructionMnemonic::BSR
        );
        debug_assert!(self.extword.get().is_some());

        Ok(self.get_extword()?.into())
    }

    /// Displacement as part of the instruction for BRA/BSR/Bcc
    pub fn get_bxx_displacement(&self) -> i32 {
        self.data as u8 as i8 as i32
    }

    /// Retrieves the data part of 'quick' instructions (except MOVEQ)
    pub fn get_quick<T: CpuSized>(&self) -> T {
        let result = T::chop(((self.data as Long) >> 9) & 0b111);
        if result == T::zero() {
            8.into()
        } else {
            result
        }
    }

    /// Retrieves the condition for a 'cc' instruction
    pub fn get_cc(&self) -> usize {
        usize::from((self.data >> 8) & 0b1111)
    }

    /// Retrieves left and right operands for EXG
    pub fn get_exg_ops(&self) -> Result<(Register, Register)> {
        let mode = (self.data >> 3) & 0b11111;
        match mode {
            0b01000 => Ok((Register::Dn(self.get_op1()), Register::Dn(self.get_op2()))),
            0b01001 => Ok((Register::An(self.get_op1()), Register::An(self.get_op2()))),
            0b10001 => Ok((Register::Dn(self.get_op1()), Register::An(self.get_op2()))),
            _ => Err(anyhow!("Invalid EXG mode: {0:b}", mode)),
        }
    }

    /// Retrieves shift count/register for the rotate/shift instructions
    pub fn get_sh_count(&self) -> Either<Long, Register> {
        let rotation = (self.data >> 9) & 0b111;
        match (self.data >> 5) & 1 {
            0 => Either::Left(if rotation == 0 { 8 } else { rotation.into() }),
            1 => Either::Right(Register::Dn(rotation.into())),
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displacement_sign() {
        // BEQ -128, -1
        let mut v = Vec::<u16>::from([0b110011110000000, 65535]);

        let i = Instruction::try_decode(|| Ok(v.remove(0))).unwrap();
        i.fetch_extword(|| Ok(v.remove(0))).unwrap();
        assert_eq!(i.get_displacement().unwrap(), -1_i32);
    }
}
