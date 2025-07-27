use anyhow::{anyhow, bail, Context, Result};
use crossbeam::atomic::AtomicCell;
use either::Either;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use proc_bitfield::bitfield;
use strum::Display;

use super::regs::Register;
use super::{CpuM68kType, CpuSized, M68000, M68010, M68020};

use crate::bus::Address;
use crate::types::{Long, Word};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstructionSize {
    /// 8-bit / 1 byte
    Byte,
    /// 16-bit / 2 bytes
    Word,
    /// 32-bit / 4 bytes
    Long,
    /// Single precision real, 32-bit / 4 bytes
    Single,
    /// Double precision real, 64-bit / 8 bytes
    Double,
    /// Extended precision real, 96-bits / 12 bytes
    Extended,
    /// Packed BCD real, 96-bits / 12 bytes
    Packed,

    None,
}

impl InstructionSize {
    pub fn bytelen(&self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Long => 4,
            Self::Single => 4,
            Self::Double => 8,
            Self::Extended => 12,
            Self::Packed => 12,
            Self::None => panic!("bytelen() on None size"),
        }
    }
}

/// Instruction mnemonic
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Display)]
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
    BFCHG,
    BFCLR,
    BFEXTU,
    BFEXTS,
    BFFFO,
    BFINS,
    BFTST,
    BFSET,
    BSET_imm,
    BTST_imm,
    // BRA is actually just Bcc with cond = True
    BSR,
    CAS_b,
    CAS_l,
    CAS_w,
    CHK_l,
    CHK_w,
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
    DIVx_l,
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
    EXTB_l,
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
    LINEA,
    LINEF,
    LINK,
    UNLINK,
    MOVE_w,
    MOVE_l,
    MOVE_b,
    MOVEA_w,
    MOVEA_l,
    MOVEC_l,
    MOVEP_w,
    MOVEP_l,
    MOVEfromCCR,
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
    MULx_l,
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
    RTD,
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

    // FPU opcodes
    FNOP,
    FSAVE,
    FRESTORE,
    FOP_000,
    FBcc_w,
    FBcc_l,
    FScc_b,
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

    // M68020 -------------------------
    /// Address Register Indirect With Index (Base Displacement)
    /// (bd, An, Xn.size*scale)
    /// Extension of IndirectIndex
    /// Uses Full Format Extension Word
    IndirectIndexBase,
    /// Program Counter Indirect With Index (Base displacement)
    PCIndexBase,
    // Memory Indirect Post-Indexed
    // ([bd,An], Xn.size*scale,od)
    //MemoryIndirectPostIndex,

    // Memory Indirect Pre-Indexed
    // ([bd,An,Xn.size*scale],od)
    //MemoryIndirectPreIndex,
}

/// I/IS Memory Indirect Actions for full extension words
#[derive(Debug, Clone, Copy)]
pub enum MemoryIndirectAction {
    None,
    PreIndexNull,
    PreIndexWord,
    PreIndexLong,
    PostIndexNull,
    PostIndexWord,
    PostIndexLong,
    Null,
    Word,
    Long,
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

/// MOVEC control register
#[derive(FromPrimitive, Debug, Eq, PartialEq, strum::Display)]
pub enum MovecCtrlReg {
    // M68010
    SFC = 0x000,
    DFC = 0x001,
    USP = 0x800,
    VBR = 0x801,
    // M68020/30/40
    CACR = 0x002,
    CAAR = 0x802,
    MSP = 0x803,
    ISP = 0x804,
}

#[allow(clippy::from_over_into)]
impl Into<(Xn, usize)> for Register {
    fn into(self) -> (Xn, usize) {
        match self {
            Self::An(n) => (Xn::An, n),
            Self::Dn(n) => (Xn::Dn, n),
            _ => panic!("Invalid conversion"),
        }
    }
}

impl From<MovecCtrlReg> for Register {
    fn from(value: MovecCtrlReg) -> Self {
        match value {
            MovecCtrlReg::SFC => Self::SFC,
            MovecCtrlReg::DFC => Self::DFC,
            MovecCtrlReg::USP => Self::USP,
            MovecCtrlReg::VBR => Self::VBR,
            MovecCtrlReg::CACR => Self::CACR,
            MovecCtrlReg::CAAR => Self::CAAR,
            MovecCtrlReg::MSP => Self::MSP,
            MovecCtrlReg::ISP => Self::ISP,
        }
    }
}

impl From<u16> for ExtWord {
    fn from(data: u16) -> Self {
        Self { data }
    }
}

impl From<ExtWord> for u16 {
    fn from(val: ExtWord) -> Self {
        val.data
    }
}

impl From<ExtWord> for u32 {
    fn from(val: ExtWord) -> Self {
        val.data as Self
    }
}

impl From<ExtWord> for i32 {
    fn from(val: ExtWord) -> Self {
        val.data as i16 as Self
    }
}

impl From<ExtWord> for usize {
    fn from(val: ExtWord) -> Self {
        val.data as Self
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

    pub fn is_full(&self) -> bool {
        self.data & (1 << 8) != 0
    }

    /// Scale - M68020+ only
    pub fn brief_get_scale(&self) -> Word {
        match (self.data >> 9) & 3 {
            0b00 => 1,
            0b01 => 2,
            0b10 => 4,
            0b11 => 8,
            _ => unreachable!(),
        }
    }

    pub fn full_base_suppress(&self) -> bool {
        self.data & (1 << 7) != 0
    }

    pub fn full_index_register(&self) -> Option<Register> {
        assert!(self.is_full());
        if self.data & (1 << 6) != 0 {
            // IS - Index Suppress
            None
        } else if self.data & (1 << 15) != 0 {
            Some(Register::An(usize::from((self.data >> 12) & 0b111)))
        } else {
            Some(Register::Dn(usize::from((self.data >> 12) & 0b111)))
        }
    }

    pub fn full_scale(&self) -> Word {
        assert!(self.is_full());
        self.brief_get_scale()
    }

    pub fn full_index_size(&self) -> IndexSize {
        assert!(self.is_full());
        self.brief_get_index_size()
    }

    pub fn full_displacement_size(&self) -> Word {
        (self.data >> 4) & 0b11
    }

    pub fn full_memindirectmode(&self) -> Result<MemoryIndirectAction> {
        assert!(self.is_full());
        let is = self.data & (1 << 6) != 0;
        let i = self.data & 0b111;

        match (is, i) {
            (_, 0b000) => Ok(MemoryIndirectAction::None),
            (false, 0b001) => Ok(MemoryIndirectAction::PreIndexNull),
            (false, 0b010) => Ok(MemoryIndirectAction::PreIndexWord),
            (false, 0b011) => Ok(MemoryIndirectAction::PreIndexLong),
            (false, 0b101) => Ok(MemoryIndirectAction::PostIndexNull),
            (false, 0b110) => Ok(MemoryIndirectAction::PostIndexWord),
            (false, 0b111) => Ok(MemoryIndirectAction::PostIndexLong),

            (true, 0b001) => Ok(MemoryIndirectAction::Null),
            (true, 0b010) => Ok(MemoryIndirectAction::Word),
            (true, 0b011) => Ok(MemoryIndirectAction::Long),

            _ => bail!(format!(
                "Invalid memory indirect mode - IS = {}, I = {:03b}",
                is, i
            )),
        }
    }
}

bitfield! {
    /// BFxxx extension word
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct BfxExtWord(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub width: Long @ 0..=4,
        pub width_reg: usize @ 0..=2,
        pub fdw: bool @ 5,
        pub offset: Long @ 6..=10,
        pub offset_reg: usize @ 6..=8,
        pub fdo: bool @ 11,
        pub reg: usize @ 12..=14,
    }
}

bitfield! {
    /// MULx.l extension word
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct MulxExtWord(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub dh: usize @ 0..=2,
        pub size: bool @ 10,
        pub signed: bool @ 11,
        pub dl: usize @ 12..=14,
    }
}

bitfield! {
    /// DIV.l/DIVS.l extension word
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct DivlExtWord(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub dr: usize @ 0..=2,
        pub size: bool @ 10,
        pub signed: bool @ 11,
        pub dq: usize @ 12..=14,
    }
}

/// A decoded instruction
#[derive(Debug)]
pub struct Instruction {
    pub mnemonic: InstructionMnemonic,
    pub data: u16,
    pub extword: AtomicCell<Option<ExtWord>>,
}

impl Clone for Instruction {
    fn clone(&self) -> Self {
        // Clone drops the loaded extension word
        // TODO rip the Cell out..
        // TODO make generic on CPU type?
        Self {
            mnemonic: self.mnemonic,
            data: self.data,
            extword: AtomicCell::new(None),
        }
    }
}

impl Instruction {
    #[rustfmt::skip]
    const DECODE_TABLE: &'static [(CpuM68kType, u16, u16, InstructionMnemonic)] = &[
        (M68000, 0b0000_0000_0011_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ORI_ccr),
        (M68000, 0b0000_0000_0111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ORI_sr),
        (M68000, 0b0000_0000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ORI_b),
        (M68000, 0b0000_0000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ORI_w),
        (M68000, 0b0000_0000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ORI_l),
        (M68000, 0b0000_0010_0011_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ANDI_ccr),
        (M68000, 0b0000_0010_0111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ANDI_sr),
        (M68000, 0b0000_0010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ANDI_b),
        (M68000, 0b0000_0010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ANDI_w),
        (M68000, 0b0000_0010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ANDI_l),
        (M68000, 0b0000_0100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::SUBI_b),
        (M68000, 0b0000_0100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::SUBI_w),
        (M68000, 0b0000_0100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::SUBI_l),
        (M68000, 0b0000_0110_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ADDI_b),
        (M68000, 0b0000_0110_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ADDI_w),
        (M68000, 0b0000_0110_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ADDI_l),
        (M68000, 0b0000_1010_0011_1100, 0b1111_1111_1111_1111, InstructionMnemonic::EORI_ccr),
        (M68000, 0b0000_1010_0111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::EORI_sr),
        (M68000, 0b0000_1010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::EORI_b),
        (M68000, 0b0000_1010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::EORI_w),
        (M68000, 0b0000_1010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::EORI_l),
        (M68000, 0b0000_1100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CMPI_b),
        (M68000, 0b0000_1100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CMPI_w),
        (M68000, 0b0000_1100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CMPI_l),
        (M68000, 0b0011_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MOVEA_w),
        (M68000, 0b0010_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MOVEA_l),
        (M68000, 0b0001_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::MOVE_b),
        (M68000, 0b0011_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::MOVE_w),
        (M68000, 0b0010_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::MOVE_l),
        (M68000, 0b0000_0001_0000_1000, 0b1111_0001_0111_1000, InstructionMnemonic::MOVEP_w),
        (M68000, 0b0000_0001_0100_1000, 0b1111_0001_0111_1000, InstructionMnemonic::MOVEP_l),
        (M68000, 0b0100_0000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEfromSR),
        (M68000, 0b0100_0100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEtoCCR),
        (M68000, 0b0100_0110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEtoSR),
        (M68000, 0b0100_1110_0110_1000, 0b1111_1111_1111_1000, InstructionMnemonic::MOVEfromUSP),
        (M68000, 0b0100_1110_0110_0000, 0b1111_1111_1111_1000, InstructionMnemonic::MOVEtoUSP),
        (M68000, 0b0100_0100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEG_b),
        (M68000, 0b0100_0100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEG_w),
        (M68000, 0b0100_0100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEG_l),
        (M68000, 0b0100_0000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEGX_b),
        (M68000, 0b0100_0000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEGX_w),
        (M68000, 0b0100_0000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NEGX_l),
        (M68000, 0b0100_0010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CLR_b),
        (M68000, 0b0100_0010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CLR_w),
        (M68000, 0b0100_0010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CLR_l),
        (M68000, 0b0100_0110_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NOT_b),
        (M68000, 0b0100_0110_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NOT_w),
        (M68000, 0b0100_0110_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NOT_l),
        (M68000, 0b0100_1001_1100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::EXTB_l),
        (M68000, 0b0100_1000_1000_0000, 0b1111_1111_1111_1000, InstructionMnemonic::EXT_w),
        (M68000, 0b0100_1000_1100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::EXT_l),
        (M68000, 0b0100_1000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::NBCD),
        (M68000, 0b0000_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BTST_dn),
        (M68000, 0b0000_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BCHG_dn),
        (M68000, 0b0000_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BCLR_dn),
        (M68000, 0b0000_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::BSET_dn),
        (M68000, 0b0000_1000_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BTST_imm),
        (M68000, 0b0000_1000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BCHG_imm),
        (M68000, 0b0000_1000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BCLR_imm),
        (M68000, 0b0000_1000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BSET_imm),
        (M68000, 0b0100_1000_0100_0000, 0b1111_1111_1111_1000, InstructionMnemonic::SWAP),
        (M68000, 0b0100_1000_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::PEA),
        (M68000, 0b0100_1010_1111_1100, 0b1111_1111_1111_1111, InstructionMnemonic::ILLEGAL),
        (M68000, 0b0100_1010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TAS),
        (M68000, 0b0100_1010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TST_b),
        (M68000, 0b0100_1010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TST_w),
        (M68000, 0b0100_1010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::TST_l),
        (M68000, 0b0100_1110_0100_0000, 0b1111_1111_1111_0000, InstructionMnemonic::TRAP),
        (M68000, 0b0100_1110_0101_0000, 0b1111_1111_1111_1000, InstructionMnemonic::LINK),
        (M68000, 0b0100_1110_0101_1000, 0b1111_1111_1111_1000, InstructionMnemonic::UNLINK),
        (M68000, 0b0100_1110_0111_0000, 0b1111_1111_1111_1111, InstructionMnemonic::RESET),
        (M68000, 0b0100_1110_0111_0001, 0b1111_1111_1111_1111, InstructionMnemonic::NOP),
        (M68000, 0b0100_1110_0111_0010, 0b1111_1111_1111_1111, InstructionMnemonic::STOP),
        (M68000, 0b0100_1110_0111_0011, 0b1111_1111_1111_1111, InstructionMnemonic::RTE),
        (M68000, 0b0100_1110_0111_0101, 0b1111_1111_1111_1111, InstructionMnemonic::RTS),
        (M68000, 0b0100_1110_0111_0110, 0b1111_1111_1111_1111, InstructionMnemonic::TRAPV),
        (M68000, 0b0100_1110_0111_0111, 0b1111_1111_1111_1111, InstructionMnemonic::RTR),
        (M68000, 0b0100_1110_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::JSR),
        (M68000, 0b0100_1110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::JMP),
        (M68000, 0b0100_1000_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_mem_w),
        (M68000, 0b0100_1000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_mem_l),
        (M68000, 0b0100_1100_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_reg_w),
        (M68000, 0b0100_1100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEM_reg_l),
        (M68000, 0b0100_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::LEA),
        (M68000, 0b0100_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CHK_w),
        (M68000, 0b1000_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SBCD),
        (M68000, 0b1000_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::OR_b),
        (M68000, 0b1000_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::OR_w),
        (M68000, 0b1000_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::OR_l),
        (M68000, 0b1000_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::DIVU_w),
        (M68000, 0b1000_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::DIVS_w),
        (M68000, 0b0101_0000_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDQ_b),
        (M68000, 0b0101_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDQ_w),
        (M68000, 0b0101_0000_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDQ_l),
        (M68000, 0b0101_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBQ_b),
        (M68000, 0b0101_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBQ_w),
        (M68000, 0b0101_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBQ_l),
        (M68000, 0b0101_0000_1100_1000, 0b1111_0000_1111_1000, InstructionMnemonic::DBcc),
        (M68000, 0b0101_0000_1100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::Scc),
        // BRA is actually just Bcc with cond = True
        (M68000, 0b0110_0001_0000_0000, 0b1111_1111_0000_0000, InstructionMnemonic::BSR),
        (M68000, 0b0110_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::Bcc),
        (M68000, 0b0111_0000_0000_0000, 0b1111_0001_0000_0000, InstructionMnemonic::MOVEQ),
        (M68000, 0b1001_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SUBX_b),
        (M68000, 0b1001_0001_0100_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SUBX_w),
        (M68000, 0b1001_0001_1000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::SUBX_l),
        (M68000, 0b1001_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::SUB_b),
        (M68000, 0b1001_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::SUB_w),
        (M68000, 0b1001_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::SUB_l),
        (M68000, 0b1001_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBA_w),
        (M68000, 0b1001_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::SUBA_l),
        (M68000, 0b1010_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::LINEA),
        (M68000, 0b1011_0001_0000_1000, 0b1111_0001_1111_1000, InstructionMnemonic::CMPM_b),
        (M68000, 0b1011_0001_0100_1000, 0b1111_0001_1111_1000, InstructionMnemonic::CMPM_w),
        (M68000, 0b1011_0001_1000_1000, 0b1111_0001_1111_1000, InstructionMnemonic::CMPM_l),
        (M68000, 0b1011_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::EOR_b),
        (M68000, 0b1011_0001_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::EOR_w),
        (M68000, 0b1011_0001_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::EOR_l),
        (M68000, 0b1011_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMPA_w),
        (M68000, 0b1011_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMPA_l),
        (M68000, 0b1011_0000_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMP_b),
        (M68000, 0b1011_0000_0100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMP_w),
        (M68000, 0b1011_0000_1000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CMP_l),
        (M68000, 0b1100_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MULU_w),
        (M68000, 0b1100_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::MULS_w),
        (M68000, 0b1100_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ABCD),
        (M68000, 0b1100_0001_0000_0000, 0b1111_0001_0011_0000, InstructionMnemonic::EXG),
        (M68000, 0b1100_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::AND_b),
        (M68000, 0b1100_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::AND_w),
        (M68000, 0b1100_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::AND_l),
        (M68000, 0b1101_0001_0000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ADDX_b),
        (M68000, 0b1101_0001_0100_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ADDX_w),
        (M68000, 0b1101_0001_1000_0000, 0b1111_0001_1111_0000, InstructionMnemonic::ADDX_l),
        (M68000, 0b1101_0000_0000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::ADD_b),
        (M68000, 0b1101_0000_0100_0000, 0b1111_0000_1100_0000, InstructionMnemonic::ADD_w),
        (M68000, 0b1101_0000_1000_0000, 0b1111_0000_1100_0000, InstructionMnemonic::ADD_l),
        (M68000, 0b1101_0000_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDA_w),
        (M68000, 0b1101_0001_1100_0000, 0b1111_0001_1100_0000, InstructionMnemonic::ADDA_l),
        (M68000, 0b1110_0000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ASR_ea),
        (M68000, 0b1110_0001_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ASL_ea),
        (M68000, 0b1110_0010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::LSR_ea),
        (M68000, 0b1110_0011_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::LSL_ea),
        (M68000, 0b1110_0100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROXR_ea),
        (M68000, 0b1110_0101_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROXL_ea),
        (M68000, 0b1110_0110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROR_ea),
        (M68000, 0b1110_0111_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::ROL_ea),
        (M68000, 0b1110_0000_0000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASR_b),
        (M68000, 0b1110_0001_0000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASL_b),
        (M68000, 0b1110_0000_0100_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASR_w),
        (M68000, 0b1110_0001_0100_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASL_w),
        (M68000, 0b1110_0000_1000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASR_l),
        (M68000, 0b1110_0001_1000_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ASL_l),
        (M68000, 0b1110_0000_0000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSR_b),
        (M68000, 0b1110_0001_0000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSL_b),
        (M68000, 0b1110_0000_0100_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSR_w),
        (M68000, 0b1110_0001_0100_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSL_w),
        (M68000, 0b1110_0000_1000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSR_l),
        (M68000, 0b1110_0001_1000_1000, 0b1111_0001_1101_1000, InstructionMnemonic::LSL_l),
        (M68000, 0b1110_0000_0001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXR_b),
        (M68000, 0b1110_0001_0001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXL_b),
        (M68000, 0b1110_0000_0101_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXR_w),
        (M68000, 0b1110_0001_0101_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXL_w),
        (M68000, 0b1110_0000_1001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXR_l),
        (M68000, 0b1110_0001_1001_0000, 0b1111_0001_1101_1000, InstructionMnemonic::ROXL_l),
        (M68000, 0b1110_0000_0001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROR_b),
        (M68000, 0b1110_0001_0001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROL_b),
        (M68000, 0b1110_0000_0101_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROR_w),
        (M68000, 0b1110_0001_0101_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROL_w),
        (M68000, 0b1110_0000_1001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROR_l),
        (M68000, 0b1110_0001_1001_1000, 0b1111_0001_1101_1000, InstructionMnemonic::ROL_l),

        // M68010+ instructions
        (M68010, 0b0100_0010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MOVEfromCCR),
        (M68010, 0b0100_1110_0111_1010, 0b1111_1111_1111_1110, InstructionMnemonic::MOVEC_l),
        (M68010, 0b0100_1110_0111_0100, 0b1111_1111_1111_1111, InstructionMnemonic::RTD),

        // M68020+ instructions
        (M68020, 0b1110_1100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFCLR),
        (M68020, 0b1110_1001_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFEXTU),
        (M68020, 0b1110_1011_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFEXTS),
        (M68020, 0b1110_1010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFCHG),
        (M68020, 0b1110_1111_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFINS),
        (M68020, 0b1110_1110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFSET),
        (M68020, 0b1110_1000_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFTST),
        (M68020, 0b1110_1101_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::BFFFO),
        (M68020, 0b0100_1100_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::MULx_l),
        (M68020, 0b0100_1100_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::DIVx_l),
        (M68020, 0b0100_0001_0000_0000, 0b1111_0001_1100_0000, InstructionMnemonic::CHK_l),
        (M68020, 0b0000_1010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CAS_b),
        (M68020, 0b0000_1100_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CAS_w),
        (M68020, 0b0000_1110_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::CAS_l),

        // M68020+ FPU instructions
        (M68020, 0b1111_0011_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::FSAVE),
        (M68020, 0b1111_0011_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::FRESTORE),
        (M68020, 0b1111_0010_1000_0000, 0b1111_1111_1111_1111, InstructionMnemonic::FNOP),
        (M68020, 0b1111_0010_0000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::FOP_000),
        (M68020, 0b1111_0010_1100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::FBcc_l),
        (M68020, 0b1111_0010_1000_0000, 0b1111_1111_1100_0000, InstructionMnemonic::FBcc_w),
        (M68020, 0b1111_0010_0100_0000, 0b1111_1111_1100_0000, InstructionMnemonic::FScc_b),
        (M68000, 0b1111_0000_0000_0000, 0b1111_0000_0000_0000, InstructionMnemonic::LINEF),
    ];

    /// Attempts to decode an instruction.
    pub fn try_decode(cpu_type: CpuM68kType, data: Word) -> Result<Self> {
        for &(_, val, mask, mnemonic) in Self::DECODE_TABLE
            .iter()
            .filter(|(t, _, _, _)| *t <= cpu_type)
        {
            if data & mask == val {
                return Ok(Self {
                    mnemonic,
                    data,
                    extword: AtomicCell::new(None),
                });
            }
        }

        Err(anyhow!("Cannot decode instruction: {:016b}", data))
    }

    /// Gets the addressing mode of this instruction
    pub fn get_addr_mode(&self) -> Result<AddressingMode> {
        debug_assert!(!matches!(
            self.mnemonic,
            InstructionMnemonic::ADDX_l
                | InstructionMnemonic::ADDX_w
                | InstructionMnemonic::ADDX_b
                | InstructionMnemonic::SUBX_l
                | InstructionMnemonic::SUBX_w
                | InstructionMnemonic::SUBX_b
                | InstructionMnemonic::ABCD
                | InstructionMnemonic::SBCD
        ));

        Self::decode_addr_mode((self.data & 0b111_000) >> 3, self.data & 0b000_111)
    }

    /// Gets the addressing mode on the 'left' side of this instruction
    /// (for MOVE)
    pub fn get_addr_mode_left(&self) -> Result<AddressingMode> {
        debug_assert!(matches!(
            self.mnemonic,
            InstructionMnemonic::MOVE_l | InstructionMnemonic::MOVE_w | InstructionMnemonic::MOVE_b
        ));

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
        // This check is to handle 'MOVE mem, (xxx).L' properly.
        if !self.has_extword() {
            self.extword.store(Some(ExtWord::from(fetch()?)));
        }
        Ok(())
    }

    pub fn clear_extword(&self) {
        self.extword.store(None);
    }

    pub fn has_extword(&self) -> bool {
        self.extword.load().is_some()
    }

    pub fn get_extword(&self) -> Result<ExtWord> {
        self.extword.load().context("Ext word not fetched")
    }

    pub fn needs_extword(&self) -> bool {
        matches!(
            self.get_addr_mode().unwrap(),
            AddressingMode::IndirectDisplacement
                | AddressingMode::IndirectIndex
                | AddressingMode::PCDisplacement
                | AddressingMode::PCIndex
        ) || matches!(self.mnemonic, InstructionMnemonic::MOVEC_l)
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
        debug_assert!(self.extword.load().is_some());

        Ok(self.get_extword()?.into())
    }

    /// Displacement as part of the instruction for BRA/BSR/Bcc
    pub fn get_bxx_displacement(&self) -> i32 {
        self.data as u8 as i8 as i32
    }

    /// Displacement as part of the instruction for BRA/BSR/Bcc
    pub fn get_bxx_displacement_raw(&self) -> u8 {
        self.data as u8
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

    /// Retrieves the condition predicate for a 'cc' instruction
    pub fn get_cc(&self) -> usize {
        usize::from((self.data >> 8) & 0b1111)
    }

    /// Retrieves the condition predicate for a 'Fcc' instruction
    pub fn get_fcc(&self) -> usize {
        usize::from(self.data & 0b111111)
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

    /// Retrieves the size of the instruction
    pub const fn get_size(&self) -> InstructionSize {
        match self.mnemonic {
            InstructionMnemonic::ADD_l
            | InstructionMnemonic::ADDA_l
            | InstructionMnemonic::ADDI_l
            | InstructionMnemonic::ADDQ_l
            | InstructionMnemonic::ADDX_l
            | InstructionMnemonic::AND_l
            | InstructionMnemonic::ANDI_l
            | InstructionMnemonic::ASL_l
            | InstructionMnemonic::ASR_l
            | InstructionMnemonic::CAS_l
            | InstructionMnemonic::CHK_l
            | InstructionMnemonic::CLR_l
            | InstructionMnemonic::CMP_l
            | InstructionMnemonic::CMPA_l
            | InstructionMnemonic::CMPI_l
            | InstructionMnemonic::CMPM_l
            | InstructionMnemonic::DIVx_l
            | InstructionMnemonic::EOR_l
            | InstructionMnemonic::EORI_l
            | InstructionMnemonic::EXT_l
            | InstructionMnemonic::LSL_l
            | InstructionMnemonic::LSR_l
            | InstructionMnemonic::OR_l
            | InstructionMnemonic::ORI_l
            | InstructionMnemonic::MOVE_l
            | InstructionMnemonic::MOVEA_l
            | InstructionMnemonic::MOVEP_l
            | InstructionMnemonic::MOVEM_mem_l
            | InstructionMnemonic::MOVEM_reg_l
            | InstructionMnemonic::MULx_l
            | InstructionMnemonic::NEG_l
            | InstructionMnemonic::NEGX_l
            | InstructionMnemonic::NOT_l
            | InstructionMnemonic::ROXL_l
            | InstructionMnemonic::ROXR_l
            | InstructionMnemonic::ROL_l
            | InstructionMnemonic::ROR_l
            | InstructionMnemonic::SUB_l
            | InstructionMnemonic::SUBA_l
            | InstructionMnemonic::SUBI_l
            | InstructionMnemonic::SUBQ_l
            | InstructionMnemonic::SUBX_l
            | InstructionMnemonic::TST_l
            | InstructionMnemonic::MOVEC_l
            | InstructionMnemonic::FBcc_l => InstructionSize::Long,

            InstructionMnemonic::ADD_w
            | InstructionMnemonic::ADDA_w
            | InstructionMnemonic::ADDI_w
            | InstructionMnemonic::ADDQ_w
            | InstructionMnemonic::ADDX_w
            | InstructionMnemonic::AND_w
            | InstructionMnemonic::ANDI_w
            | InstructionMnemonic::ASL_w
            | InstructionMnemonic::ASR_w
            | InstructionMnemonic::CAS_w
            | InstructionMnemonic::CLR_w
            | InstructionMnemonic::CMP_w
            | InstructionMnemonic::CMPA_w
            | InstructionMnemonic::CMPI_w
            | InstructionMnemonic::CMPM_w
            | InstructionMnemonic::DIVS_w
            | InstructionMnemonic::DIVU_w
            | InstructionMnemonic::EOR_w
            | InstructionMnemonic::EORI_w
            | InstructionMnemonic::EXT_w
            | InstructionMnemonic::LSL_w
            | InstructionMnemonic::LSR_w
            | InstructionMnemonic::OR_w
            | InstructionMnemonic::ORI_w
            | InstructionMnemonic::MOVE_w
            | InstructionMnemonic::MOVEA_w
            | InstructionMnemonic::MOVEP_w
            | InstructionMnemonic::MOVEM_mem_w
            | InstructionMnemonic::MOVEM_reg_w
            | InstructionMnemonic::MOVEfromCCR
            | InstructionMnemonic::MULU_w
            | InstructionMnemonic::MULS_w
            | InstructionMnemonic::NEG_w
            | InstructionMnemonic::NEGX_w
            | InstructionMnemonic::NOT_w
            | InstructionMnemonic::ROXL_w
            | InstructionMnemonic::ROXR_w
            | InstructionMnemonic::ROL_w
            | InstructionMnemonic::ROR_w
            | InstructionMnemonic::SUB_w
            | InstructionMnemonic::SUBA_w
            | InstructionMnemonic::SUBI_w
            | InstructionMnemonic::SUBQ_w
            | InstructionMnemonic::SUBX_w
            | InstructionMnemonic::TST_w
            | InstructionMnemonic::FBcc_w => InstructionSize::Word,

            InstructionMnemonic::ADD_b
            | InstructionMnemonic::ADDI_b
            | InstructionMnemonic::ADDQ_b
            | InstructionMnemonic::ADDX_b
            | InstructionMnemonic::AND_b
            | InstructionMnemonic::ANDI_b
            | InstructionMnemonic::ASL_b
            | InstructionMnemonic::ASR_b
            | InstructionMnemonic::CAS_b
            | InstructionMnemonic::CLR_b
            | InstructionMnemonic::CMP_b
            | InstructionMnemonic::CMPI_b
            | InstructionMnemonic::CMPM_b
            | InstructionMnemonic::EOR_b
            | InstructionMnemonic::EORI_b
            | InstructionMnemonic::EXTB_l
            | InstructionMnemonic::LSL_b
            | InstructionMnemonic::LSR_b
            | InstructionMnemonic::OR_b
            | InstructionMnemonic::ORI_b
            | InstructionMnemonic::MOVE_b
            | InstructionMnemonic::NEG_b
            | InstructionMnemonic::NEGX_b
            | InstructionMnemonic::NOT_b
            | InstructionMnemonic::ROXL_b
            | InstructionMnemonic::ROXR_b
            | InstructionMnemonic::ROL_b
            | InstructionMnemonic::ROR_b
            | InstructionMnemonic::SUB_b
            | InstructionMnemonic::SUBI_b
            | InstructionMnemonic::SUBQ_b
            | InstructionMnemonic::SUBX_b
            | InstructionMnemonic::TST_b
            | InstructionMnemonic::ABCD
            | InstructionMnemonic::NBCD
            | InstructionMnemonic::SBCD
            | InstructionMnemonic::FScc_b
            | InstructionMnemonic::ANDI_ccr
            | InstructionMnemonic::EORI_ccr
            | InstructionMnemonic::ORI_ccr
            | InstructionMnemonic::MOVEtoCCR => InstructionSize::Byte,

            InstructionMnemonic::ANDI_sr
            | InstructionMnemonic::ORI_sr
            | InstructionMnemonic::EORI_sr
            | InstructionMnemonic::MOVEfromSR
            | InstructionMnemonic::MOVEtoSR => InstructionSize::Word,

            InstructionMnemonic::MOVEfromUSP | InstructionMnemonic::MOVEtoUSP => {
                InstructionSize::Long
            }
            InstructionMnemonic::ASL_ea
            | InstructionMnemonic::ASR_ea
            | InstructionMnemonic::LSL_ea
            | InstructionMnemonic::LSR_ea
            | InstructionMnemonic::ROXL_ea
            | InstructionMnemonic::ROXR_ea
            | InstructionMnemonic::ROL_ea
            | InstructionMnemonic::ROR_ea => InstructionSize::Word,

            InstructionMnemonic::BCHG_dn
            | InstructionMnemonic::BCLR_dn
            | InstructionMnemonic::BSET_dn
            | InstructionMnemonic::BTST_dn => InstructionSize::Long,
            InstructionMnemonic::BCHG_imm
            | InstructionMnemonic::BCLR_imm
            | InstructionMnemonic::BSET_imm
            | InstructionMnemonic::BTST_imm => InstructionSize::Byte,

            InstructionMnemonic::Bcc
            | InstructionMnemonic::BFEXTU
            | InstructionMnemonic::BFEXTS
            | InstructionMnemonic::BFFFO
            | InstructionMnemonic::BFCHG
            | InstructionMnemonic::BFCLR
            | InstructionMnemonic::BFINS
            | InstructionMnemonic::BFSET
            | InstructionMnemonic::BFTST
            | InstructionMnemonic::BSR
            | InstructionMnemonic::CHK_w
            | InstructionMnemonic::DBcc
            | InstructionMnemonic::EXG
            | InstructionMnemonic::FOP_000
            | InstructionMnemonic::FNOP
            | InstructionMnemonic::FRESTORE
            | InstructionMnemonic::FSAVE
            | InstructionMnemonic::ILLEGAL
            | InstructionMnemonic::JMP
            | InstructionMnemonic::JSR
            | InstructionMnemonic::NOP
            | InstructionMnemonic::LEA
            | InstructionMnemonic::LINEA
            | InstructionMnemonic::LINEF
            | InstructionMnemonic::LINK
            | InstructionMnemonic::UNLINK
            | InstructionMnemonic::MOVEQ
            | InstructionMnemonic::PEA
            | InstructionMnemonic::RESET
            | InstructionMnemonic::RTD
            | InstructionMnemonic::RTE
            | InstructionMnemonic::RTR
            | InstructionMnemonic::RTS
            | InstructionMnemonic::Scc
            | InstructionMnemonic::STOP
            | InstructionMnemonic::SWAP
            | InstructionMnemonic::TAS
            | InstructionMnemonic::TRAP
            | InstructionMnemonic::TRAPV => InstructionSize::None,
        }
    }

    /// Is this considered a branch instruction?
    pub fn is_branch(&self) -> bool {
        self.mnemonic == InstructionMnemonic::JSR || self.mnemonic == InstructionMnemonic::BSR
    }

    /// MOVEC dr field
    pub fn movec_ctrl_to_gen(&self) -> bool {
        debug_assert!(self.mnemonic == InstructionMnemonic::MOVEC_l);
        (self.data & 1) == 0
    }

    /// MOVEC general register
    pub fn movec_reg(&self) -> Result<Register> {
        debug_assert!(self.mnemonic == InstructionMnemonic::MOVEC_l);

        let extword = usize::from(self.get_extword()?);

        let regnum = (extword >> 12) & 0b111;
        Ok(if extword & (1 << 15) == 0 {
            Register::Dn(regnum)
        } else {
            Register::An(regnum)
        })
    }

    /// MOVEC control register
    pub fn movec_ctrlreg(&self) -> Result<MovecCtrlReg> {
        debug_assert!(self.mnemonic == InstructionMnemonic::MOVEC_l);
        MovecCtrlReg::from_u16(u16::from(self.get_extword()?) & 0xFFF)
            .context("Invalid control register")
    }

    /// Displacement for IndirectIndex modes with full extension words
    pub fn fetch_ind_full_displacement<F>(&self, mut fetch: F) -> Result<i32>
    where
        F: FnMut() -> Result<u16>,
    {
        let extword = self.get_extword()?;
        assert!(extword.is_full());
        //assert_eq!(self.get_addr_mode()?, AddressingMode::IndirectIndex);

        // Base displacement size
        match extword.full_displacement_size() {
            0b00 => bail!("Reserved displacement size?"),
            0b01 => Ok(0),
            0b10 => Ok(fetch()? as i16 as i32),
            0b11 => {
                let msb = fetch()? as u32;
                let lsb = fetch()? as u32;
                Ok(((msb << 16) | lsb) as i32)
            }
            _ => unreachable!(),
        }
    }
}

impl std::fmt::Display for Instruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.mnemonic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displacement_sign() {
        // BEQ -128, -1
        let mut v = Vec::<u16>::from([0b110011110000000, 65535]);

        let i = Instruction::try_decode(M68000, v.remove(0)).unwrap();
        i.fetch_extword(|| Ok(v.remove(0))).unwrap();
        assert_eq!(i.get_displacement().unwrap(), -1_i32);
    }
}
