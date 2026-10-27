use std::fmt;

use anyhow::Result;
use arrayvec::ArrayVec;

use super::Variant;
#[cfg(feature = "w65816")]
use super::instruction_table::TABLE_65816;
use super::instruction_table::{TABLE_65C02, TABLE_6502};

pub const MAX_INSTRUCTION_LEN: usize = 4;

/// Instruction addressing mode
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum AddressingMode {
    /// Absolute
    /// 3 bytes, $OP $LL $HH
    Absolute,
    /// Absolute,X
    /// 3 bytes, $OP $LL $HH
    AbsoluteX,
    /// Absolute,Y
    /// 3 bytes, $OP $LL $HH
    AbsoluteY,
    /// (Absolute)
    /// 3 bytes, $OP $LL $HH
    AbsolutePtr16,
    /// [Absolute]
    /// 3 bytes, $OP $LL $HH
    #[cfg(feature = "w65816")]
    AbsolutePtr24,
    /// (Absolute,X)
    /// 3 bytes, $OP $LL $HH
    AbsoluteXPtr16,
    /// Accumulator
    /// 1 byte, $OP
    Accumulator,
    /// DIRECT
    /// 2 bytes, $OP $LL
    Direct,
    /// DIRECT,X
    /// 2 bytes, $OP $LL
    DirectX,
    /// DIRECT,Y
    /// 2 bytes, $OP $LL
    DirectY,
    /// (DIRECT)
    /// 2 bytes, $OP $LL
    DirectPtr16,
    /// [DIRECT]
    /// 2 bytes, $OP $LL
    #[cfg(feature = "w65816")]
    DirectPtr24,
    /// (DIRECT,X)
    /// 2 bytes, $OP $LL
    DirectXPtr16,
    /// (DIRECT),Y
    /// 2 bytes, $OP $LL
    DirectPtr16Y,
    /// [DIRECT],Y
    /// 2 bytes, $OP $LL
    #[cfg(feature = "w65816")]
    DirectPtr24Y,
    /// DIRECT,relative (65C02 BBR/BBS)
    /// 3 bytes, $OP $LL $RR
    DirectRelative,
    /// Immediate (8-bit)
    /// Length: 2 bytes, $OP $LL (for 8-bit data)
    Immediate8,
    /// Immediate (16-bit)
    /// 3 bytes, $OP $LL $HH
    #[cfg(feature = "w65816")]
    Immediate16,
    /// Immediate (depending on M-flag)
    /// 2-3 bytes, $OP $LL ($HH)
    #[cfg(feature = "w65816")]
    ImmediateM,
    /// Immediate (depending on X-flag)
    /// 2-3 bytes, $OP $LL ($HH)
    #[cfg(feature = "w65816")]
    ImmediateX,
    /// Implied
    /// 1 byte, $OP
    Implied,
    /// Long
    /// 4 bytes, $OP $LL $MM $HH
    #[cfg(feature = "w65816")]
    Long,
    /// Long,X
    /// 4 bytes, $OP $LL $MM $HH
    #[cfg(feature = "w65816")]
    LongX,
    /// Relative 8-bit
    /// 2 bytes, $OP $LL
    Relative8,
    /// Relative 16-bit
    /// 2 bytes, $OP $LL $HH
    #[cfg(feature = "w65816")]
    Relative16,
    /// Source,Desination
    /// 3 bytes, $OP $TT $SS
    #[cfg(feature = "w65816")]
    SrcDest,
    /// Stack,S
    /// 2 bytes, $OP $LL
    #[cfg(feature = "w65816")]
    StackS,
    /// (Stack,S),Y
    /// 2 bytes, $OP $LL
    #[cfg(feature = "w65816")]
    StackSPtr16Y,
}

impl AddressingMode {
    // m and x only matter to the 65816's ImmediateM/ImmediateX.
    #[cfg_attr(not(feature = "w65816"), allow(unused_variables))]
    pub fn get_fetch_len(&self, m: bool, x: bool) -> usize {
        match self {
            Self::Absolute => 3,
            Self::AbsoluteX => 3,
            Self::AbsoluteY => 3,
            Self::AbsolutePtr16 => 3,
            #[cfg(feature = "w65816")]
            Self::AbsolutePtr24 => 3,
            Self::AbsoluteXPtr16 => 3,
            Self::Accumulator => 1,
            Self::Direct => 2,
            Self::DirectX => 2,
            Self::DirectY => 2,
            Self::DirectPtr16 => 2,
            #[cfg(feature = "w65816")]
            Self::DirectPtr24 => 2,
            Self::DirectXPtr16 => 2,
            Self::DirectPtr16Y => 2,
            #[cfg(feature = "w65816")]
            Self::DirectPtr24Y => 2,
            Self::DirectRelative => 3,
            Self::Immediate8 => 2,
            #[cfg(feature = "w65816")]
            Self::Immediate16 => 3,
            Self::Implied => 1,
            #[cfg(feature = "w65816")]
            Self::Long => 4,
            #[cfg(feature = "w65816")]
            Self::LongX => 4,
            Self::Relative8 => 2,
            #[cfg(feature = "w65816")]
            Self::Relative16 => 3,
            #[cfg(feature = "w65816")]
            Self::SrcDest => 3,
            #[cfg(feature = "w65816")]
            Self::StackS => 2,
            #[cfg(feature = "w65816")]
            Self::StackSPtr16Y => 2,

            #[cfg(feature = "w65816")]
            Self::ImmediateM => 3 - usize::from(m),
            #[cfg(feature = "w65816")]
            Self::ImmediateX => 3 - usize::from(x),
        }
    }
}

/// Instruction types
#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub enum InstructionType {
    #[cfg(feature = "w65816")]
    MVN,
    #[cfg(feature = "w65816")]
    MVP,
    TYA,
    #[cfg(feature = "w65816")]
    TYX,
    CMP,
    DEC,
    #[cfg(feature = "w65816")]
    SEP,
    #[cfg(feature = "w65816")]
    XBA,
    SEI,
    SEC,
    JMP,
    SED,
    DEX,
    DEY,
    ORA,
    #[cfg(feature = "w65816")]
    PHD,
    PHA,
    #[cfg(feature = "w65816")]
    PHB,
    INX,
    INY,
    AND,
    #[cfg(feature = "w65816")]
    PHK,
    SBC,
    INC,
    TAX,
    TAY,
    PHP,
    PHY,
    TRB,
    PHX,
    TSB,
    #[cfg(feature = "w65816")]
    TSC,
    #[cfg(feature = "w65816")]
    WDM,
    CLV,
    #[cfg(feature = "w65816")]
    XCE,
    CLC,
    CLD,
    TSX,
    CLI,
    ADC,
    BMI,
    BEQ,
    BRK,
    #[cfg(feature = "w65816")]
    BRL,
    BRA,
    JSR,
    ROL,
    #[cfg(feature = "w65816")]
    COP,
    STA,
    STZ,
    STX,
    STY,
    LSR,
    STP,
    ROR,
    #[cfg(feature = "w65816")]
    JSL,
    ASL,
    #[cfg(feature = "w65816")]
    TCS,
    #[cfg(feature = "w65816")]
    TCD,
    PLY,
    PLX,
    PLP,
    PLA,
    NOP,
    #[cfg(feature = "w65816")]
    PLB,
    #[cfg(feature = "w65816")]
    PLD,
    BCC,
    BPL,
    #[cfg(feature = "w65816")]
    TDC,
    BCS,
    #[cfg(feature = "w65816")]
    PEA,
    RTS,
    WAI,
    #[cfg(feature = "w65816")]
    PEI,
    #[cfg(feature = "w65816")]
    PER,
    RTI,
    #[cfg(feature = "w65816")]
    RTL,
    LDY,
    LDX,
    LDA,
    BVC,
    BVS,
    BNE,
    CPX,
    CPY,
    #[cfg(feature = "w65816")]
    REP,
    TXA,
    TXS,
    #[cfg(feature = "w65816")]
    TXY,
    EOR,
    BIT,
    /// 65C02: reset memory bit (RMB0–7)
    RMB,
    /// 65C02: set memory bit (SMB0–7)
    SMB,
    /// 65C02: branch on bit reset (BBR0–7)
    BBR,
    /// 65C02: branch on bit set (BBS0–7)
    BBS,
    Undefined,
}

/// A definition in the instruction (op code) table
pub struct InstructionDef {
    /// String representation
    /// '@' represents an immediate value to be replacee
    pub mnemonic: &'static str,

    /// Addressing mode
    pub mode: AddressingMode,

    /// Length
    pub len: usize,

    /// Instruction type
    pub instr_type: InstructionType,
}

/// A decoded instruction
#[derive(Clone)]
pub struct Instruction {
    /// Reference to definition in instruction table
    pub def: &'static InstructionDef,

    /// Immediate values
    pub immediate: [u32; 2],

    /// Raw bytes
    pub raw: ArrayVec<u8, MAX_INSTRUCTION_LEN>,

    /// Instruction length
    pub len: usize,
}

impl Instruction {
    /// Try to decode a single instruction from an iterator.
    pub fn decode(
        stream: &mut impl Iterator<Item = u8>,
        m: bool,
        x: bool,
        variant: Variant,
    ) -> Option<Self> {
        let mut raw: ArrayVec<u8, MAX_INSTRUCTION_LEN> = ArrayVec::new();
        let mut rd = || -> Option<u8> {
            // Assuming the stream wraps at the end, no errors will be thrown.
            let b = stream.next()?;
            raw.push(b);
            Some(b)
        };

        let opcode = rd()?;
        let table = match variant {
            Variant::M6502 => &TABLE_6502,
            Variant::M65C02 => &TABLE_65C02,
            #[cfg(feature = "w65816")]
            Variant::W65816 => &TABLE_65816,
        };
        let def = &table[opcode as usize];
        let mut args = [0; MAX_INSTRUCTION_LEN];
        let len = def.mode.get_fetch_len(m, x);
        for arg in args.iter_mut().take(len - 1) {
            *arg = rd()?;
        }

        // Transform immediate values
        let mut immediate = [0; 2];
        match def.mode {
            #[cfg(feature = "w65816")]
            AddressingMode::SrcDest => {
                immediate[0] = args[1] as u32;
                immediate[1] = args[0] as u32;
            }
            AddressingMode::DirectRelative => {
                immediate[0] = args[0] as u32;
                immediate[1] = args[1] as u32;
            }
            _ => immediate[0] = u32::from_le_bytes(args),
        }

        Some(Self {
            def,
            immediate,
            raw,
            len,
        })
    }

    /// Reads the immediate value for addressing modes that
    /// have one immediate value.
    pub fn imm<T: std::convert::TryFrom<u32>>(&self) -> Result<T>
    where
        anyhow::Error: From<T::Error>,
    {
        #[cfg(feature = "w65816")]
        assert_ne!(self.def.mode, AddressingMode::SrcDest);
        assert_ne!(self.def.mode, AddressingMode::Implied);
        assert_ne!(self.def.mode, AddressingMode::Accumulator);
        Ok(self.immediate[0].try_into()?)
    }

    /// Reads the immediate value for src/dest addressing mode.
    #[cfg(feature = "w65816")]
    pub fn imm_srcdest<T: std::convert::TryFrom<u32>>(&self) -> Result<(T, T)>
    where
        anyhow::Error: From<T::Error>,
    {
        assert_eq!(self.def.mode, AddressingMode::SrcDest);
        Ok((self.immediate[0].try_into()?, self.immediate[1].try_into()?))
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut s = self.def.mnemonic.to_string();

        #[cfg(feature = "w65816")]
        if self.def.mode == AddressingMode::SrcDest {
            s = s.replacen('@', format!("${:02X}", self.immediate[0]).as_str(), 1);
            s = s.replacen('@', format!("${:02X}", self.immediate[1]).as_str(), 1);
            return write!(f, "{:02X?} {}", self.raw, s.as_str());
        }

        match self.def.len - 1 {
            1 => s = s.replacen('@', format!("${:02X}", self.immediate[0]).as_str(), 1),
            2 => s = s.replacen('@', format!("${:04X}", self.immediate[0]).as_str(), 1),
            3 => s = s.replacen('@', format!("${:06X}", self.immediate[0]).as_str(), 1),
            _ => (),
        }

        write!(f, "{:02X?} {}", self.raw, s.as_str())
    }
}
