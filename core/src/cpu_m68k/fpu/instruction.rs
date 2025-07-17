use num_derive::{FromPrimitive, ToPrimitive};
use proc_bitfield::bitfield;

use crate::cpu_m68k::instruction::InstructionSize;
use crate::cpu_m68k::regs::Register;
use crate::types::Word;

#[allow(clippy::upper_case_acronyms)]
#[derive(FromPrimitive, strum::Display, strum::EnumIter, ToPrimitive)]
pub(in crate::cpu_m68k) enum FmoveControlReg {
    // The order here is relevant!
    FPCR = 0b100,
    FPSR = 0b010,
    FPIAR = 0b001,
}

impl From<FmoveControlReg> for Register {
    fn from(value: FmoveControlReg) -> Self {
        match value {
            FmoveControlReg::FPCR => Self::FPCR,
            FmoveControlReg::FPSR => Self::FPSR,
            FmoveControlReg::FPIAR => Self::FPIAR,
        }
    }
}

bitfield! {
    /// FMOVE extension word
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub(in crate::cpu_m68k) struct FmoveExtWord(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        /// MOVECR ROM offset
        pub movecr_offset: usize @ 0..=6,

        /// Sub-operation bits
        pub subop: u8 @ 13..=15,

        /// (Control register) Register select
        pub reg: u8 @ 10..=12,

        /// (EA to register) Register select
        pub dst_reg: usize @ 7..=9,

        /// (EA to register) Source specifier
        pub src_spec: u8 @ 10..=12,

        /// (EA to register) Opmode
        pub opmode: u8 @ 0..=6,

        /// (Register to EA) Destination format
        pub dest_fmt: u8 @ 10..=12,

        /// (Register to EA) Source register
        pub src_reg: usize @ 7..=9,

        /// (Register to EA) K-factor
        pub k_factor: u8 @ 0..=6,

        /// (FMOVEM) Direction: 1=register to EA, 0=EA to register
        pub movem_dir: bool @ 13,

        /// (FMOVEM) Register list mask
        pub movem_reglist: u8 @ 0..=7,

        /// (FMOVEM) Mode field
        pub movem_mode: u8 @ 11..=12,
    }
}

impl FmoveExtWord {
    pub fn src_spec_instrsz(&self) -> Option<InstructionSize> {
        match self.src_spec() {
            0b000 => Some(InstructionSize::Long),
            0b001 => Some(InstructionSize::Single),
            0b010 => Some(InstructionSize::Extended),
            0b011 => Some(InstructionSize::Packed),
            0b100 => Some(InstructionSize::Word),
            0b101 => Some(InstructionSize::Double),
            0b110 => Some(InstructionSize::Byte),
            _ => None,
        }
    }

    pub fn dest_fmt_instrsz(&self) -> Option<InstructionSize> {
        match self.dest_fmt() {
            0b000 => Some(InstructionSize::Long),
            0b001 => Some(InstructionSize::Single),
            0b010 => Some(InstructionSize::Extended),
            0b011 | 0b111 => Some(InstructionSize::Packed),
            0b100 => Some(InstructionSize::Word),
            0b101 => Some(InstructionSize::Double),
            0b110 => Some(InstructionSize::Byte),
            _ => None,
        }
    }
}
