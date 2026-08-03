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

/// Opmodes of the FPU ALU operations
pub(in crate::cpu_m68k) mod opmode {
    /// Amount of distinct opmodes (7 bits)
    pub(in crate::cpu_m68k) const COUNT: usize = 128;

    macro_rules! opmodes {
        ($($name:ident = $value:expr, $mnemonic:literal;)*) => {
            $(pub(in crate::cpu_m68k) const $name: u8 = $value;)*

            /// Mnemonic of an ALU opmode, for the disassembler
            pub(in crate::cpu_m68k) fn mnemonic(opmode: u8) -> &'static str {
                match opmode {
                    $($name => $mnemonic,)*
                    _ => "F???",
                }
            }
        };
    }

    opmodes! {
        FMOVE   = 0b0000000, "FMOVE";
        FINT    = 0b0000001, "FINT";
        FSINH   = 0b0000010, "FSINH";
        FINTRZ  = 0b0000011, "FINTRZ";
        FSQRT   = 0b0000100, "FSQRT";
        FLOGNP1 = 0b0000110, "FLOGNP1";
        FETOXM1 = 0b0001000, "FETOXM1";
        FTANH   = 0b0001001, "FTANH";
        FATAN   = 0b0001010, "FATAN";
        FASIN   = 0b0001100, "FASIN";
        FATANH  = 0b0001101, "FATANH";
        FSIN    = 0b0001110, "FSIN";
        FTAN    = 0b0001111, "FTAN";
        FETOX   = 0b0010000, "FETOX";
        FTWOTOX = 0b0010001, "FTWOTOX";
        FTENTOX = 0b0010010, "FTENTOX";
        FLOGN   = 0b0010100, "FLOGN";
        FLOG10  = 0b0010101, "FLOG10";
        FLOG2   = 0b0010110, "FLOG2";
        FABS    = 0b0011000, "FABS";
        FCOSH   = 0b0011001, "FCOSH";
        FNEG    = 0b0011010, "FNEG";
        FACOS   = 0b0011100, "FACOS";
        FCOS    = 0b0011101, "FCOS";
        FGETEXP = 0b0011110, "FGETEXP";
        FGETMAN = 0b0011111, "FGETMAN";
        FDIV    = 0b0100000, "FDIV";
        FMOD    = 0b0100001, "FMOD";
        FADD    = 0b0100010, "FADD";
        FMUL    = 0b0100011, "FMUL";
        FSGLDIV = 0b0100100, "FSGLDIV";
        FREM    = 0b0100101, "FREM";
        FSCALE  = 0b0100110, "FSCALE";
        FSGLMUL = 0b0100111, "FSGLMUL";
        FSUB    = 0b0101000, "FSUB";
        FCMP    = 0b0111000, "FCMP";
        FTST    = 0b0111010, "FTST";
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
        pub k_factor: i8 @ 0..=6,

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
