use std::fmt;

use anyhow::Result;
use arrayvec::ArrayVec;

use super::instruction_table::INSTRUCTION_TABLE;

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
    DirectPtr24,
    /// (DIRECT,X)
    /// 2 bytes, $OP $LL
    DirectXPtr16,
    /// (DIRECT),Y
    /// 2 bytes, $OP $LL
    DirectPtr16Y,
    /// [DIRECT],Y
    /// 2 bytes, $OP $LL
    DirectPtr24Y,
    /// Immediate (8-bit)
    /// Length: 2 bytes, $OP $LL (for 8-bit data)
    Immediate8,
    /// Immediate (16-bit)
    /// 3 bytes, $OP $LL $HH
    Immediate16,
    /// Immediate (depending on M-flag)
    /// 2-3 bytes, $OP $LL ($HH)
    ImmediateM,
    /// Immediate (depending on X-flag)
    /// 2-3 bytes, $OP $LL ($HH)
    ImmediateX,
    /// Implied
    /// 1 byte, $OP
    Implied,
    /// Long
    /// 4 bytes, $OP $LL $MM $HH
    Long,
    /// Long,X
    /// 4 bytes, $OP $LL $MM $HH
    LongX,
    /// Relative 8-bit
    /// 2 bytes, $OP $LL
    Relative8,
    /// Relative 16-bit
    /// 2 bytes, $OP $LL $HH
    Relative16,
    /// Source,Desination
    /// 3 bytes, $OP $TT $SS
    SrcDest,
    /// Stack,S
    /// 2 bytes, $OP $LL
    StackS,
    /// (Stack,S),Y
    /// 2 bytes, $OP $LL
    StackSPtr16Y,
}

impl AddressingMode {
    pub fn get_fetch_len(&self, m: bool, x: bool) -> usize {
        let m = if m { 1 } else { 0 };
        let x = if x { 1 } else { 0 };

        match self {
            Self::Absolute => 3,
            Self::AbsoluteX => 3,
            Self::AbsoluteY => 3,
            Self::AbsolutePtr16 => 3,
            Self::AbsolutePtr24 => 3,
            Self::AbsoluteXPtr16 => 3,
            Self::Accumulator => 1,
            Self::Direct => 2,
            Self::DirectX => 2,
            Self::DirectY => 2,
            Self::DirectPtr16 => 2,
            Self::DirectPtr24 => 2,
            Self::DirectXPtr16 => 2,
            Self::DirectPtr16Y => 2,
            Self::DirectPtr24Y => 2,
            Self::Immediate8 => 2,
            Self::Immediate16 => 3,
            Self::Implied => 1,
            Self::Long => 4,
            Self::LongX => 4,
            Self::Relative8 => 2,
            Self::Relative16 => 3,
            Self::SrcDest => 3,
            Self::StackS => 2,
            Self::StackSPtr16Y => 2,

            Self::ImmediateM => 3 - m,
            Self::ImmediateX => 3 - x,
        }
    }
}

/// Instruction types
#[derive(Eq, PartialEq)]
pub enum InstructionType {
    MVN,
    MVP,
    TYA,
    TYX,
    CMP,
    DEC,
    SEP,
    XBA,
    SEI,
    SEC,
    JMP,
    SED,
    DEX,
    DEY,
    ORA,
    PHD,
    PHA,
    PHB,
    INX,
    INY,
    AND,
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
    TSC,
    WDM,
    CLV,
    XCE,
    CLC,
    CLD,
    TSX,
    CLI,
    ADC,
    BMI,
    BEQ,
    BRK,
    BRL,
    BRA,
    JSR,
    ROL,
    COP,
    STA,
    STZ,
    STX,
    STY,
    LSR,
    STP,
    ROR,
    JSL,
    ASL,
    TCS,
    TCD,
    PLY,
    PLX,
    PLP,
    PLA,
    NOP,
    PLB,
    PLD,
    BCC,
    BPL,
    TDC,
    BCS,
    PEA,
    RTS,
    WAI,
    PEI,
    PER,
    RTI,
    RTL,
    LDY,
    LDX,
    LDA,
    BVC,
    BVS,
    BNE,
    CPX,
    CPY,
    REP,
    TXA,
    TXS,
    TXY,
    EOR,
    BIT,
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
    pub fn decode(stream: &mut impl Iterator<Item = u8>, m: bool, x: bool) -> Option<Instruction> {
        let mut raw: ArrayVec<u8, MAX_INSTRUCTION_LEN> = ArrayVec::new();
        let mut rd = || -> Option<u8> {
            // Assuming the stream wraps at the end, no errors will be thrown.
            let b = stream.next()?;
            raw.push(b);
            Some(b)
        };

        let opcode = rd()?;
        let def = &INSTRUCTION_TABLE[opcode as usize];
        let mut args = [0; MAX_INSTRUCTION_LEN];
        let len = def.mode.get_fetch_len(m, x);
        for i in 0..(len - 1) {
            args[i] = rd()?;
        }

        // Transform immediate values
        let mut immediate = [0; 2];
        match def.mode {
            AddressingMode::SrcDest => {
                immediate[0] = args[1] as u32;
                immediate[1] = args[0] as u32;
            }
            _ => immediate[0] = u32::from_le_bytes(args),
        }

        Some(Instruction {
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
        assert_ne!(self.def.mode, AddressingMode::SrcDest);
        assert_ne!(self.def.mode, AddressingMode::Implied);
        assert_ne!(self.def.mode, AddressingMode::Accumulator);
        Ok(self.immediate[0].try_into()?)
    }

    /// Reads the immediate value for src/dest addressing mode.
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

        match self.def.mode {
            AddressingMode::SrcDest => {
                s = s.replacen('@', format!("${:02X}", self.immediate[0]).as_str(), 1);
                s = s.replacen('@', format!("${:02X}", self.immediate[1]).as_str(), 1);
            }
            _ => match self.def.len - 1 {
                1 => s = s.replacen('@', format!("${:02X}", self.immediate[0]).as_str(), 1),
                2 => s = s.replacen('@', format!("${:04X}", self.immediate[0]).as_str(), 1),
                3 => s = s.replacen('@', format!("${:06X}", self.immediate[0]).as_str(), 1),
                _ => (),
            },
        }

        write!(f, "{:02X?} {}", self.raw, s.as_str())
    }
}
