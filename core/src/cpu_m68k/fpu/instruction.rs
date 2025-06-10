use num_derive::FromPrimitive;
use proc_bitfield::bitfield;

use crate::cpu_m68k::regs::Register;
use crate::types::Word;

#[allow(clippy::upper_case_acronyms)]
#[derive(FromPrimitive, strum::Display)]
pub(in crate::cpu_m68k) enum FmoveControlReg {
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

        /// (FMOVEM) Direction: 0=register to EA, 1=EA to register
        pub movem_dir: bool @ 13,

        /// (FMOVEM) Register list mask
        pub movem_reglist: u8 @ 0..=7,

        /// (FMOVEM) Mode field
        pub movem_mode: u8 @ 11..=12,
    }
}
