use anyhow::{bail, Context, Result};
use arrayvec::ArrayVec;
use either::Either;
use itertools::Itertools;
use num_traits::ToPrimitive;
use strum::IntoEnumIterator;

use std::fmt::Write;

use crate::bus::Address;
use crate::cpu_m68k::instruction::{
    BfxExtWord, DivlExtWord, IndexSize, MemoryIndirectAction, MulxExtWord, Xn,
};
use crate::cpu_m68k::pmmu::instruction::PtestExtword;
use crate::cpu_m68k::regs::Register;
use crate::types::{Byte, Word};

use super::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use super::instruction::{
    AddressingMode, Direction, Instruction, InstructionMnemonic, InstructionSize,
};
use super::pmmu::instruction::Pmove1Extword;
use super::CpuM68kType;

#[derive(Clone)]
pub struct DisassemblyEntry {
    pub addr: Address,
    pub raw: ArrayVec<u8, 20>,
    pub str: String,
}

impl DisassemblyEntry {
    pub fn raw_as_string(&self) -> String {
        self.raw.iter().fold(String::new(), |mut output, b| {
            let _ = write!(output, "{b:02X}");
            output
        })
    }

    pub fn opcode(&self) -> Word {
        ((self.raw[0] as Word) << 8) | (self.raw[1] as Word)
    }

    pub fn is_linea(&self) -> bool {
        self.raw.len() == 2 && self.raw[0] & 0xF0 == 0xA0
    }
}

impl std::fmt::Display for DisassemblyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            ":{:08X} {:<16} {}",
            self.addr,
            self.raw_as_string(),
            self.str
        )
    }
}

pub struct Disassembler<'a> {
    /// Input iterator
    iter: &'a mut dyn Iterator<Item = u8>,

    /// Current absolute address
    addr: Address,

    /// Current entry that is being worked on
    out: DisassemblyEntry,
}

impl<'a> Disassembler<'a> {
    const CC: &'static [&'static str; 16] = &[
        "T", "F", "HI", "LS", "CC", "CS", "NE", "EQ", "VC", "VS", "PL", "MI", "GE", "LT", "GT",
        "LE",
    ];
    const MOVEM_REGS: &'static [&'static str; 16] = &[
        "A7", "A6", "A5", "A4", "A3", "A2", "A1", "A0", "D7", "D6", "D5", "D4", "D3", "D2", "D1",
        "D0",
    ];

    /// Returns the mnemonic abbreviation for FPU condition codes
    fn fcc_mnemonic(cc: usize) -> &'static str {
        match cc & 0b111111 {
            // Miscellaneous Tests
            0b000000 => "F",   // False
            0b001111 => "T",   // True
            0b010000 => "SF",  // Signaling False
            0b011111 => "ST",  // Signaling True
            0b010001 => "SEQ", // Signaling Equal
            0b011110 => "SNE", // Signaling Not Equal

            // IEEE Aware Tests
            0b000001 => "EQ",  // Equal
            0b001110 => "NE",  // Not Equal
            0b000010 => "OGT", // Ordered Greater Than
            0b001101 => "ULE", // Unordered or Less or Equal
            0b000011 => "OGE", // Ordered Greater Than or Equal
            0b001100 => "ULT", // Unordered or Less Than
            0b000100 => "OLT", // Ordered Less Than
            0b001011 => "UGE", // Unordered or Greater or Equal
            0b000101 => "OLE", // Ordered Less Than or Equal
            0b001010 => "UGT", // Unordered or Greater Than
            0b000110 => "OGL", // Ordered Greater or Less Than
            0b001001 => "UEQ", // Unordered or Equal
            0b000111 => "OR",  // Ordered
            0b001000 => "UN",  // Unordered

            // IEEE Nonaware Tests
            0b010010 => "GT",   // Greater Than
            0b011101 => "NGT",  // Not Greater Than
            0b010011 => "GE",   // Greater Than or Equal
            0b011100 => "NGE",  // Not (Greater Than or Equal)
            0b010100 => "LT",   // Less Than
            0b011011 => "NLT",  // Not Less Than
            0b010101 => "LE",   // Less Than or Equal
            0b011010 => "NLE",  // Not (Less Than or Equal)
            0b010110 => "GL",   // Greater or Less Than
            0b011001 => "NGL",  // Not (Greater or Less Than)
            0b010111 => "GLE",  // Greater, Less or Equal
            0b011000 => "NGLE", // Not (Greater, Less or Equal)

            _ => "???", // Unknown condition
        }
    }

    fn fpu_alu_op(op: u8) -> &'static str {
        match op {
            0b0000000 => "FMOVE",
            0b0000100 => "FSQRT",
            0b0011000 => "FABS",
            0b0100010 => "FADD",
            0b0101000 => "FSUB",
            0b0100011 => "FMUL",
            0b0100000 => "FDIV",
            0b0000001 => "FINT",
            0b0000011 => "FINTRZ",
            0b0111000 => "FCMP",
            0b0100101 => "FREM",
            0b0111010 => "FTST",
            0b0011010 => "FNEG",
            0b0011101 => "FCOS",
            0b0001010 => "FATAN",
            0b0011110 => "FGETEXP",
            0b0001110 => "FSIN",
            0b0001111 => "FTAN",
            0b0010100 => "FLOGN",
            0b0010000 => "FETOX",
            0b0010101 => "FLOG10",
            0b0001000 => "FETOXM1",
            0b0010110 => "FLOG2",
            0b0000110 => "FLOGNP1",
            0b0010001 => "FTWOTOX",
            0b0010010 => "FTENTOX",
            0b0000010 => "FSINH",
            0b0011001 => "FCOSH",
            0b0001001 => "FTANH",
            0b0001101 => "FATANH",
            0b0100001 => "FMOD",
            0b0100110 => "FSCALE",
            0b0011111 => "FGETMAN",
            0b0100111 => "FSGLMUL",
            0b0100100 => "FSGLDIV",

            _ => "F???",
        }
    }

    pub fn from(iter: &'a mut dyn Iterator<Item = u8>, addr: Address) -> Self {
        Self {
            addr,
            iter,
            out: DisassemblyEntry {
                addr,
                raw: ArrayVec::new(),
                str: String::default(),
            },
        }
    }

    fn get8(&mut self) -> Result<u8> {
        let data = self.iter.next().context("Premature end of stream")?;
        self.out.raw.push(data);
        Ok(data)
    }

    fn get16(&mut self) -> Result<u16> {
        let msb = self.get8()?;
        let lsb = self.get8()?;
        Ok(((msb as u16) << 8) | (lsb as u16))
    }

    fn get32(&mut self) -> Result<u32> {
        let upper = self.get16()?;
        let lower = self.get16()?;
        Ok(((upper as u32) << 16) | (lower as u32))
    }

    fn get_n(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut r = Vec::with_capacity(n);
        for _ in 0..n {
            r.push(self.get8()?);
        }
        Ok(r)
    }

    /// Format FPU register list for FMOVEM instruction
    fn format_fpu_reglist(&self, reglist: u8, reverse: bool) -> String {
        let regs = ["FP0", "FP1", "FP2", "FP3", "FP4", "FP5", "FP6", "FP7"];

        let regiter = if reverse {
            Either::Left(regs.iter())
        } else {
            Either::Right(regs.iter().rev())
        };

        regiter
            .enumerate()
            .filter(|(i, _)| reglist & (1 << i) != 0)
            .map(|(_, r)| *r)
            .collect::<Vec<_>>()
            .join("/")
    }

    fn ea(&mut self, instr: &Instruction) -> Result<String> {
        self.ea_with(instr, instr.get_addr_mode()?, instr.get_op2())
    }

    fn ea_sz(&mut self, instr: &Instruction, sz: InstructionSize) -> Result<String> {
        self.ea_with_sz(instr, instr.get_addr_mode()?, instr.get_op2(), sz)
    }

    fn ea_left(&mut self, instr: &Instruction) -> Result<String> {
        self.ea_with(instr, instr.get_addr_mode_left()?, instr.get_op1())
    }

    fn ea_with(&mut self, instr: &Instruction, mode: AddressingMode, op: usize) -> Result<String> {
        self.ea_with_sz(instr, mode, op, instr.get_size())
    }

    fn ea_with_sz(
        &mut self,
        instr: &Instruction,
        mode: AddressingMode,
        op: usize,
        sz: InstructionSize,
    ) -> Result<String> {
        instr.clear_extword();
        Ok(match mode {
            AddressingMode::Immediate => match sz {
                InstructionSize::Byte => format!("#${:02X}", self.get16()?),
                InstructionSize::Word => format!("#${:04X}", self.get16()?),
                InstructionSize::Long => format!("#${:08X}", self.get32()?),
                InstructionSize::Single
                | InstructionSize::Double
                | InstructionSize::Extended
                | InstructionSize::Packed => format!("{:?}", self.get_n(sz.bytelen())?),
                InstructionSize::None => bail!("Invalid addr mode"),
            },
            AddressingMode::DataRegister => format!("D{}", op),
            AddressingMode::AddressRegister => format!("A{}", op),
            AddressingMode::Indirect => format!("(A{})", op),
            AddressingMode::IndirectPreDec => format!("-(A{})", op),
            AddressingMode::IndirectPostInc => format!("(A{})+", op),
            AddressingMode::IndirectDisplacement => {
                instr.fetch_extword(|| self.get16())?;
                format!("(${:04X},A{})", instr.get_displacement()?, op)
            }
            AddressingMode::AbsoluteShort => format!("(${:04X})", self.get16()?),
            AddressingMode::AbsoluteLong => format!("(${:08X})", self.get32()?),
            AddressingMode::PCDisplacement => {
                instr.fetch_extword(|| self.get16())?;
                format!(
                    "${:08X}",
                    self.addr.wrapping_add_signed(instr.get_displacement()? + 2)
                )
            }
            AddressingMode::IndirectIndex => {
                instr.fetch_extword(|| self.get16())?;
                let extword = instr.get_extword()?;

                if extword.is_full() {
                    // AddressingMode::IndirectIndexBase and friends
                    let displacement = instr.fetch_ind_full_displacement(|| self.get16())?;
                    let basereg = if extword.full_base_suppress() {
                        "-".to_string()
                    } else {
                        Register::An(op).to_string()
                    };
                    let indexreg = extword
                        .full_index_register()
                        .map(|r| {
                            format!(
                                "{}.{}*{}",
                                r,
                                match extword.full_index_size() {
                                    IndexSize::Word => "w",
                                    IndexSize::Long => "l",
                                },
                                extword.full_scale()
                            )
                        })
                        .unwrap_or_else(|| "-".to_string());

                    match extword.full_memindirectmode()? {
                        MemoryIndirectAction::None => {
                            format!("(${:04X},{},{})", displacement, basereg, indexreg)
                        }
                        MemoryIndirectAction::PreIndexNull => {
                            format!("([${:04X},{},{}])", displacement, basereg, indexreg)
                        }
                        MemoryIndirectAction::PreIndexWord => {
                            let od = self.get16()?;
                            format!(
                                "([${:04X},{},{}],${:04X})",
                                displacement, basereg, indexreg, od
                            )
                        }
                        MemoryIndirectAction::PreIndexLong => {
                            let od = self.get32()?;
                            format!(
                                "([${:04X},{},{}],${:08X})",
                                displacement, basereg, indexreg, od
                            )
                        }
                        MemoryIndirectAction::PostIndexNull => {
                            format!("([${:04X},{}],{})", displacement, basereg, indexreg)
                        }
                        MemoryIndirectAction::PostIndexWord => {
                            let od = self.get16()?;
                            format!(
                                "([${:04X},{}],{},${:04X})",
                                displacement, basereg, indexreg, od
                            )
                        }
                        MemoryIndirectAction::PostIndexLong => {
                            let od = self.get32()?;
                            format!(
                                "([${:04X},{}],{},${:08X})",
                                displacement, basereg, indexreg, od
                            )
                        }
                        MemoryIndirectAction::Null => {
                            format!("([${:04X},{}])", displacement, basereg)
                        }
                        MemoryIndirectAction::Word => {
                            let od = self.get16()?;
                            format!("([${:04X},{}],${:04X})", displacement, basereg, od)
                        }
                        MemoryIndirectAction::Long => {
                            let od = self.get32()?;
                            format!("([${:04X},{}],${:08X})", displacement, basereg, od)
                        }
                    }
                } else {
                    let (xn, reg) = extword.brief_get_register();
                    format!(
                        "(${:04X},A{},{}{}.{})",
                        extword.brief_get_displacement_signext(),
                        op,
                        match xn {
                            Xn::Dn => "D",
                            Xn::An => "A",
                        },
                        reg,
                        match extword.brief_get_index_size() {
                            IndexSize::Word => "w",
                            IndexSize::Long => "l",
                        }
                    )
                }
            }
            _ => format!("{:?}", mode),
        })
    }

    fn fmove_ctrl_regs(&self, regs: u8) -> String {
        FmoveControlReg::iter()
            .filter(|fc| regs & fc.to_u8().unwrap() != 0)
            .map(|fc| fc.to_string())
            .join("+")
    }

    fn do_instr(&mut self, instr: &Instruction) -> Result<()> {
        let mnemonic = instr
            .mnemonic
            .to_string()
            .chars()
            .take_while(|&c| c != '_')
            .collect::<String>();
        let sz = instr.mnemonic.to_string().chars().last().unwrap();

        self.out.str = match instr.mnemonic {
            InstructionMnemonic::ILLEGAL
            | InstructionMnemonic::NOP
            | InstructionMnemonic::STOP
            | InstructionMnemonic::RESET
            | InstructionMnemonic::RTE
            | InstructionMnemonic::RTR
            | InstructionMnemonic::RTS
            | InstructionMnemonic::TRAPV => mnemonic,

            InstructionMnemonic::Bcc | InstructionMnemonic::BSR => {
                let displacement = if instr.get_bxx_displacement() == 0 {
                    self.get16()? as i16 as i32
                } else if instr.get_bxx_displacement_raw() == 0xFF {
                    self.get32()? as i32
                } else {
                    instr.get_bxx_displacement()
                };
                format!(
                    "B{}.{} {:08X}",
                    if instr.mnemonic == InstructionMnemonic::Bcc {
                        Self::CC[instr.get_cc()]
                    } else {
                        "SR"
                    },
                    if instr.get_bxx_displacement_raw() == 0xFF {
                        'l'
                    } else if instr.get_bxx_displacement() == 0 {
                        'w'
                    } else {
                        'b'
                    },
                    self.addr.wrapping_add_signed(displacement + 2)
                )
            }

            InstructionMnemonic::DBcc => {
                let displacement = self.get16()? as i16 as i32;
                format!(
                    "DB{} D{},${:08X}",
                    Self::CC[instr.get_cc()],
                    instr.get_op2(),
                    self.addr.wrapping_add_signed(displacement + 2)
                )
            }

            InstructionMnemonic::Scc => {
                format!("S{} {}", Self::CC[instr.get_cc()], self.ea(instr)?)
            }

            InstructionMnemonic::MOVEM_reg_w | InstructionMnemonic::MOVEM_reg_l => {
                let mask = self.get16()?;

                let regs = if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
                    Either::Left(Self::MOVEM_REGS.iter().rev())
                } else {
                    Either::Right(Self::MOVEM_REGS.iter())
                };

                format!(
                    "MOVEM.{} {}, [{}]",
                    sz,
                    self.ea(instr)?,
                    regs.enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, r)| r)
                        .join("/")
                )
            }

            InstructionMnemonic::MOVEM_mem_w | InstructionMnemonic::MOVEM_mem_l => {
                let mask = self.get16()?;

                let regs = if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
                    Either::Left(Self::MOVEM_REGS.iter().rev())
                } else {
                    Either::Right(Self::MOVEM_REGS.iter())
                };

                format!(
                    "MOVEM.{} [{}], {}",
                    sz,
                    regs.enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, r)| r)
                        .join("/"),
                    self.ea(instr)?
                )
            }

            InstructionMnemonic::CMP_l
            | InstructionMnemonic::CMP_w
            | InstructionMnemonic::CMP_b => format!(
                "{}.{} {},D{}",
                mnemonic,
                sz,
                self.ea(instr)?,
                instr.get_op1()
            ),

            InstructionMnemonic::CMPI_l
            | InstructionMnemonic::CMPI_w
            | InstructionMnemonic::CMPI_b => {
                format!(
                    "{}.{} {},{}",
                    mnemonic,
                    sz,
                    match instr.get_size() {
                        InstructionSize::Long => format!("#${:08X}", self.get32()?),
                        InstructionSize::Word => format!("#${:04X}", self.get16()?),
                        InstructionSize::Byte => format!("#${:02X}", self.get16()?),
                        _ => unreachable!(),
                    },
                    self.ea(instr)?
                )
            }

            InstructionMnemonic::LEA => {
                format!("{} {},A{}", mnemonic, self.ea(instr)?, instr.get_op1())
            }

            InstructionMnemonic::JMP | InstructionMnemonic::JSR => {
                let target = match instr.get_addr_mode()? {
                    AddressingMode::AbsoluteShort => {
                        format!("${:04X}", self.get16()?)
                    }
                    AddressingMode::AbsoluteLong => {
                        format!("${:08X}", self.get32()?)
                    }
                    _ => self.ea(instr)?,
                };
                format!("{} {}", mnemonic, target)
            }

            InstructionMnemonic::MOVEfromCCR => format!("MOVE.w CCR,{}", self.ea(instr)?),
            InstructionMnemonic::MOVEtoCCR => format!("MOVE.w {},CCR", self.ea(instr)?),
            InstructionMnemonic::MOVEtoSR => format!("MOVE.w {},SR", self.ea(instr)?),
            InstructionMnemonic::MOVEfromSR => format!("MOVE.w SR,{}", self.ea(instr)?),
            InstructionMnemonic::MOVEfromUSP => format!("MOVE.l USP,A{}", instr.get_op2()),
            InstructionMnemonic::MOVEtoUSP => format!("MOVE.l A{},USP", instr.get_op2()),
            InstructionMnemonic::MOVEQ => format!(
                "{} #${:02X},D{}",
                mnemonic,
                instr.data as u8,
                instr.get_op1()
            ),

            InstructionMnemonic::TST_l
            | InstructionMnemonic::TST_w
            | InstructionMnemonic::TST_b
            | InstructionMnemonic::NOT_l
            | InstructionMnemonic::NOT_w
            | InstructionMnemonic::NOT_b => format!("{}.{} {}", mnemonic, sz, self.ea(instr)?),

            InstructionMnemonic::AND_l
            | InstructionMnemonic::AND_w
            | InstructionMnemonic::AND_b
            | InstructionMnemonic::EOR_l
            | InstructionMnemonic::EOR_w
            | InstructionMnemonic::EOR_b
            | InstructionMnemonic::OR_l
            | InstructionMnemonic::OR_w
            | InstructionMnemonic::OR_b
            | InstructionMnemonic::ADD_l
            | InstructionMnemonic::ADD_w
            | InstructionMnemonic::ADD_b
            | InstructionMnemonic::SUB_l
            | InstructionMnemonic::SUB_w
            | InstructionMnemonic::SUB_b => {
                let left = instr.get_op1();
                let right = self.ea(instr)?;
                match instr.get_direction() {
                    Direction::Left => format!("{}.{} D{},{}", mnemonic, sz, left, right),
                    Direction::Right => format!("{}.{} {},D{}", mnemonic, sz, right, left),
                }
            }

            InstructionMnemonic::ADDA_l
            | InstructionMnemonic::ADDA_w
            | InstructionMnemonic::SUBA_l
            | InstructionMnemonic::SUBA_w => {
                let left = instr.get_op1();
                let right = self.ea(instr)?;
                match instr.get_direction() {
                    Direction::Left => format!("{}.{} A{},{}", mnemonic, sz, left, right),
                    Direction::Right => format!("{}.{} {},A{}", mnemonic, sz, right, left),
                }
            }

            InstructionMnemonic::ADDI_l
            | InstructionMnemonic::ADDI_w
            | InstructionMnemonic::ADDI_b
            | InstructionMnemonic::ANDI_l
            | InstructionMnemonic::ANDI_w
            | InstructionMnemonic::ANDI_b
            | InstructionMnemonic::EORI_l
            | InstructionMnemonic::EORI_w
            | InstructionMnemonic::EORI_b
            | InstructionMnemonic::ORI_l
            | InstructionMnemonic::ORI_w
            | InstructionMnemonic::ORI_b
            | InstructionMnemonic::SUBI_l
            | InstructionMnemonic::SUBI_w
            | InstructionMnemonic::SUBI_b => {
                format!(
                    "{}.{} {},{}",
                    mnemonic,
                    sz,
                    match instr.get_size() {
                        InstructionSize::Long => format!("#${:08X}", self.get32()?),
                        InstructionSize::Word => format!("#${:04X}", self.get16()?),
                        InstructionSize::Byte => format!("#${:02X}", self.get16()?),
                        _ => unreachable!(),
                    },
                    self.ea(instr)?
                )
            }

            InstructionMnemonic::ADDQ_l
            | InstructionMnemonic::ADDQ_w
            | InstructionMnemonic::ADDQ_b
            | InstructionMnemonic::SUBQ_l
            | InstructionMnemonic::SUBQ_w
            | InstructionMnemonic::SUBQ_b => format!(
                "{}.{} #${:02X},{}",
                mnemonic,
                sz,
                instr.get_quick::<Byte>(),
                self.ea(instr)?
            ),
            InstructionMnemonic::MOVE_w
            | InstructionMnemonic::MOVE_l
            | InstructionMnemonic::MOVE_b => {
                let src = self.ea(instr)?;
                let dest = self.ea_left(instr)?;
                format!("{}.{} {},{}", mnemonic, sz, src, dest)
            }

            InstructionMnemonic::MOVEA_w | InstructionMnemonic::MOVEA_l => format!(
                "{}.{} {},A{}",
                mnemonic,
                sz,
                self.ea(instr)?,
                instr.get_op1()
            ),

            InstructionMnemonic::DIVS_w | InstructionMnemonic::DIVU_w => {
                let left = instr.get_op1();
                let right = self.ea(instr)?;
                format!("{} {},D{}", mnemonic, right, left)
            }

            InstructionMnemonic::BCHG_dn
            | InstructionMnemonic::BCLR_dn
            | InstructionMnemonic::BSET_dn
            | InstructionMnemonic::BTST_dn => {
                format!("{} D{},{}", mnemonic, instr.get_op1(), self.ea(instr)?)
            }

            InstructionMnemonic::BCHG_imm
            | InstructionMnemonic::BCLR_imm
            | InstructionMnemonic::BSET_imm
            | InstructionMnemonic::BTST_imm => {
                let bit = self.get16()?;
                let ea = self.ea(instr)?;
                format!("{} #{},{}", mnemonic, bit, ea)
            }

            InstructionMnemonic::CLR_l
            | InstructionMnemonic::CLR_w
            | InstructionMnemonic::CLR_b
            | InstructionMnemonic::NBCD
            | InstructionMnemonic::NEG_l
            | InstructionMnemonic::NEG_w
            | InstructionMnemonic::NEG_b
            | InstructionMnemonic::NEGX_l
            | InstructionMnemonic::NEGX_w
            | InstructionMnemonic::NEGX_b => format!("{}.{} {}", mnemonic, sz, self.ea(instr)?),

            InstructionMnemonic::EXT_l
            | InstructionMnemonic::EXT_w
            | InstructionMnemonic::EXTB_l => {
                format!("{}.{} D{}", mnemonic, sz, instr.get_op2())
            }

            InstructionMnemonic::EXG => {
                let (l, r) = instr.get_exg_ops()?;
                format!("{} {},{}", mnemonic, l, r)
            }

            InstructionMnemonic::ABCD
            | InstructionMnemonic::ADDX_l
            | InstructionMnemonic::ADDX_w
            | InstructionMnemonic::ADDX_b
            | InstructionMnemonic::SUBX_l
            | InstructionMnemonic::SUBX_w
            | InstructionMnemonic::SUBX_b
            | InstructionMnemonic::SBCD => {
                format!(
                    "{}.{} {},{}",
                    mnemonic,
                    sz,
                    self.ea_with(instr, instr.get_addr_mode_x()?, instr.get_op2())?,
                    self.ea_with(instr, instr.get_addr_mode_x()?, instr.get_op1())?,
                )
            }

            InstructionMnemonic::LINEA | InstructionMnemonic::LINEF => {
                format!("{} [${:04X}]", mnemonic, instr.data)
            }

            InstructionMnemonic::MOVEP_w | InstructionMnemonic::MOVEP_l => {
                instr.fetch_extword(|| self.get16())?;
                let addr = format!("(A{}+${:04X})", instr.get_op2(), instr.get_displacement()?);
                match instr.get_direction_movep() {
                    Direction::Left => format!("{}.{} {},D{}", mnemonic, sz, addr, instr.get_op1()),
                    Direction::Right => {
                        format!("{}.{} D{},{}", mnemonic, sz, instr.get_op1(), addr)
                    }
                }
            }

            InstructionMnemonic::ANDI_ccr
            | InstructionMnemonic::EORI_ccr
            | InstructionMnemonic::ORI_ccr => format!("{} #${:02X},CCR", mnemonic, self.get16()?,),

            InstructionMnemonic::ANDI_sr
            | InstructionMnemonic::EORI_sr
            | InstructionMnemonic::ORI_sr => format!("{} #${:04X},SR", mnemonic, self.get16()?),

            InstructionMnemonic::ASL_b
            | InstructionMnemonic::ASL_w
            | InstructionMnemonic::ASL_l
            | InstructionMnemonic::ASR_b
            | InstructionMnemonic::ASR_w
            | InstructionMnemonic::ASR_l
            | InstructionMnemonic::LSL_b
            | InstructionMnemonic::LSL_w
            | InstructionMnemonic::LSL_l
            | InstructionMnemonic::LSR_b
            | InstructionMnemonic::LSR_w
            | InstructionMnemonic::LSR_l
            | InstructionMnemonic::ROXL_b
            | InstructionMnemonic::ROXL_w
            | InstructionMnemonic::ROXL_l
            | InstructionMnemonic::ROXR_b
            | InstructionMnemonic::ROXR_w
            | InstructionMnemonic::ROXR_l
            | InstructionMnemonic::ROL_b
            | InstructionMnemonic::ROL_w
            | InstructionMnemonic::ROL_l
            | InstructionMnemonic::ROR_b
            | InstructionMnemonic::ROR_w
            | InstructionMnemonic::ROR_l => {
                let count = match instr.get_sh_count() {
                    Either::Left(i) => format!("#{}", i),
                    Either::Right(r) => r.to_string(),
                };
                format!("{}.{} {},D{}", mnemonic, sz, count, instr.get_op2())
            }

            InstructionMnemonic::ASL_ea
            | InstructionMnemonic::ASR_ea
            | InstructionMnemonic::LSL_ea
            | InstructionMnemonic::LSR_ea
            | InstructionMnemonic::ROXL_ea
            | InstructionMnemonic::ROXR_ea
            | InstructionMnemonic::ROL_ea
            | InstructionMnemonic::ROR_ea => format!("{}.w {}", mnemonic, self.ea(instr)?),

            InstructionMnemonic::CHK_w | InstructionMnemonic::CHK_l => {
                format!("{} {},D{}", mnemonic, self.ea(instr)?, instr.get_op1())
            }

            InstructionMnemonic::CMPA_l | InstructionMnemonic::CMPA_w => {
                format!(
                    "{}.{} {},A{}",
                    mnemonic,
                    sz,
                    self.ea(instr)?,
                    instr.get_op1()
                )
            }

            InstructionMnemonic::CMPM_l
            | InstructionMnemonic::CMPM_w
            | InstructionMnemonic::CMPM_b => {
                let left = self.ea_with(instr, AddressingMode::IndirectPostInc, instr.get_op2())?;
                let right =
                    self.ea_with(instr, AddressingMode::IndirectPostInc, instr.get_op1())?;
                format!("{}.{} {},{}", mnemonic, sz, left, right)
            }

            InstructionMnemonic::LINK_w => {
                instr.fetch_extword(|| self.get16())?;
                format!(
                    "{}.{} A{},#{}",
                    mnemonic,
                    sz,
                    instr.get_op2(),
                    instr.get_displacement()?
                )
            }

            InstructionMnemonic::LINK_l => {
                let displacement = self.get32()?;
                format!("{}.{} A{},#{}", mnemonic, sz, instr.get_op2(), displacement)
            }

            InstructionMnemonic::UNLINK => format!("{} A{}", mnemonic, instr.get_op2()),

            InstructionMnemonic::PEA | InstructionMnemonic::TAS => {
                format!("{} {}", mnemonic, self.ea(instr)?)
            }

            InstructionMnemonic::SWAP => format!("{} D{}", mnemonic, instr.get_op2()),

            InstructionMnemonic::MULU_w | InstructionMnemonic::MULS_w => {
                format!(
                    "{}.{} {},D{}",
                    mnemonic,
                    sz,
                    self.ea(instr)?,
                    instr.get_op1()
                )
            }

            InstructionMnemonic::TRAP => format!("{} #{}", mnemonic, instr.trap_get_vector()),

            // M68010+ -----------------------------------------------------------------------------
            InstructionMnemonic::MOVEC_l => {
                instr.fetch_extword(|| self.get16())?;

                let (left, right) = if instr.movec_ctrl_to_gen() {
                    (
                        instr.movec_ctrlreg()?.to_string(),
                        instr.movec_reg()?.to_string(),
                    )
                } else {
                    (
                        instr.movec_reg()?.to_string(),
                        instr.movec_ctrlreg()?.to_string(),
                    )
                };
                format!("{} {},{}", mnemonic, left, right)
            }
            InstructionMnemonic::RTD => {
                let displacement = self.get16()? as i16;
                format!("{} #{}", mnemonic, displacement)
            }
            InstructionMnemonic::BFCHG => {
                let sec = BfxExtWord(self.get16()?);

                let offset = if sec.fdo() {
                    format!("D{}", sec.offset_reg())
                } else {
                    sec.offset().to_string()
                };
                let width = if sec.fdw() {
                    format!("D{}", sec.width_reg())
                } else if sec.width() == 0 {
                    "32".to_string()
                } else {
                    sec.width().to_string()
                };

                format!("{} {} {{{}:{}}}", mnemonic, self.ea(instr)?, offset, width,)
            }
            InstructionMnemonic::BFEXTU
            | InstructionMnemonic::BFEXTS
            | InstructionMnemonic::BFFFO
            | InstructionMnemonic::BFSET
            | InstructionMnemonic::BFCLR
            | InstructionMnemonic::BFTST => {
                let sec = BfxExtWord(self.get16()?);

                let offset = if sec.fdo() {
                    format!("D{}", sec.offset_reg())
                } else {
                    sec.offset().to_string()
                };
                let width = if sec.fdw() {
                    format!("D{}", sec.width_reg())
                } else if sec.width() == 0 {
                    "32".to_string()
                } else {
                    sec.width().to_string()
                };

                format!(
                    "{} {} {{{}:{}}}, D{}",
                    mnemonic,
                    self.ea(instr)?,
                    offset,
                    width,
                    sec.reg()
                )
            }
            InstructionMnemonic::BFINS => {
                let sec = BfxExtWord(self.get16()?);

                let offset = if sec.fdo() {
                    format!("D{}", sec.offset_reg())
                } else {
                    sec.offset().to_string()
                };
                let width = if sec.fdw() {
                    format!("D{}", sec.width_reg())
                } else if sec.width() == 0 {
                    "32".to_string()
                } else {
                    sec.width().to_string()
                };

                format!(
                    "{} D{}, {} {{{}:{}}}",
                    mnemonic,
                    sec.reg(),
                    self.ea(instr)?,
                    offset,
                    width,
                )
            }
            InstructionMnemonic::MULx_l => {
                let ew = MulxExtWord(self.get16()?);

                let regs = if ew.size() {
                    format!("D{}-D{}", ew.dh(), ew.dl())
                } else {
                    format!("D{}", ew.dl())
                };

                format!(
                    "MUL{}.{} {},{}",
                    if ew.signed() { "S" } else { "" },
                    sz,
                    self.ea(instr)?,
                    regs
                )
            }
            InstructionMnemonic::DIVx_l => {
                let ew = DivlExtWord(self.get16()?);

                let regs = if ew.size() {
                    format!("D{}:D{}", ew.dr(), ew.dq())
                } else {
                    format!("D{}", ew.dq())
                };

                format!(
                    "DIV{}.l {},{}",
                    if ew.signed() { "S" } else { "" },
                    self.ea(instr)?,
                    regs
                )
            }
            InstructionMnemonic::FNOP => {
                self.get16()?;
                instr.mnemonic.to_string()
            }
            InstructionMnemonic::FSAVE | InstructionMnemonic::FRESTORE => {
                format!("{} {}", instr.mnemonic, self.ea(instr)?)
            }
            InstructionMnemonic::FOP_000 => {
                let extword = FmoveExtWord(self.get16()?);
                match extword.subop() {
                    // FMOVE/ALU op from FPx to FPx
                    0b000 => format!(
                        "{}.x FP{},FP{}",
                        Self::fpu_alu_op(extword.opmode()),
                        extword.src_spec(),
                        extword.dst_reg()
                    ),
                    0b100 => format!(
                        "FMOVE {},{}",
                        self.ea_sz(instr, InstructionSize::Long)?,
                        self.fmove_ctrl_regs(extword.reg()),
                    ),
                    0b101 => format!(
                        "FMOVE {},{}",
                        self.fmove_ctrl_regs(extword.reg()),
                        self.ea_sz(instr, InstructionSize::Long)?,
                    ),
                    0b010 if extword.src_spec() == 0b111 => format!(
                        "FMOVECR #${:02X},FP{}",
                        extword.movecr_offset(),
                        extword.dst_reg()
                    ),
                    // FMOVE/ALU op from EA to FPx
                    0b010 => format!(
                        "{}.{} {},FP{}",
                        Self::fpu_alu_op(extword.opmode()),
                        match extword.src_spec() {
                            0b000 => "l",
                            0b001 => "s",
                            0b010 => "x",
                            0b011 => "p",
                            0b100 => "w",
                            0b101 => "d",
                            0b110 => "b",
                            _ => "?",
                        },
                        self.ea_sz(
                            instr,
                            extword.src_spec_instrsz().context("Invalid src spec")?
                        )?,
                        extword.dst_reg()
                    ),
                    0b011 => format!(
                        "FMOVE.{} FP{},{}",
                        match extword.dest_fmt() {
                            0b000 => "l",
                            0b001 => "s",
                            0b010 => "x",
                            0b011 => "p",
                            0b100 => "w",
                            0b101 => "d",
                            0b110 => "b",
                            _ => "?",
                        },
                        extword.src_reg(),
                        self.ea_sz(
                            instr,
                            extword.dest_fmt_instrsz().context("Invalid dest fmt")?
                        )?,
                    ),
                    0b110 | 0b111 => {
                        // FMOVEM - Multiple register move
                        let reglist = extword.movem_reglist();
                        let mode = extword.movem_mode();

                        match mode {
                            0b00 | 0b10 => {
                                // Static register list
                                let reg_str = self.format_fpu_reglist(
                                    reglist,
                                    instr.get_addr_mode()? == AddressingMode::IndirectPreDec,
                                );
                                if !extword.movem_dir() {
                                    // EA to registers
                                    format!("FMOVEM.x {},{}", self.ea(instr)?, reg_str)
                                } else {
                                    // Registers to EA
                                    format!("FMOVEM.x {},{}", reg_str, self.ea(instr)?)
                                }
                            }
                            0b01 | 0b11 => {
                                // Dynamic register list (from control register)
                                let ctrl_reg = match extword.reg() {
                                    0b001 => "FPIAR",
                                    0b010 => "FPSR",
                                    0b100 => "FPCR",
                                    _ => "???",
                                };
                                if extword.movem_dir() {
                                    format!("FMOVEM.x {},D{}", self.ea(instr)?, ctrl_reg)
                                } else {
                                    format!("FMOVEM.x D{},{}", ctrl_reg, self.ea(instr)?)
                                }
                            }
                            _ => "FMOVEM ???".to_string(),
                        }
                    }
                    _ => format!("{} ???", instr.mnemonic),
                }
            }

            InstructionMnemonic::FBcc_l => {
                let displacement = self.get32()? as i32;
                format!(
                    "FB{}.l ${:08X}",
                    Self::fcc_mnemonic(instr.get_fcc()),
                    displacement
                )
            }
            InstructionMnemonic::FBcc_w => {
                let displacement = self.get16()? as i16;
                format!(
                    "FB{}.w ${:04X}",
                    Self::fcc_mnemonic(instr.get_fcc()),
                    displacement
                )
            }
            InstructionMnemonic::FScc_b => {
                let cc = usize::from(self.get16()? & 0b111111);
                format!("FS{}.b {}", Self::fcc_mnemonic(cc), self.ea(instr)?)
            }

            InstructionMnemonic::CAS_b
            | InstructionMnemonic::CAS_l
            | InstructionMnemonic::CAS_w => {
                if instr.data & 0b111111 == 0b111100 {
                    // CAS2
                    let extword1 = self.get16()?;
                    let extword2 = self.get16()?;

                    let rn1 = if extword1 & (1 << 15) != 0 {
                        Register::An(usize::from((extword1 >> 12) & 0b111))
                    } else {
                        Register::Dn(usize::from((extword1 >> 12) & 0b111))
                    };
                    let rn2 = if extword2 & (1 << 15) != 0 {
                        Register::An(usize::from((extword2 >> 12) & 0b111))
                    } else {
                        Register::Dn(usize::from((extword2 >> 12) & 0b111))
                    };
                    let du1 = usize::from((extword1 >> 6) & 0b111);
                    let du2 = usize::from((extword2 >> 6) & 0b111);
                    let dc1 = usize::from(extword1 & 0b111);
                    let dc2 = usize::from(extword2 & 0b111);
                    format!(
                        "{}2.{} D{}:D{},D{}:D{},({}):({})",
                        mnemonic, sz, dc1, dc2, du1, du2, rn1, rn2
                    )
                } else {
                    // CAS
                    let extword = self.get16()?;
                    let dc = (extword & 0b111) as usize;
                    let du = ((extword >> 6) & 0b111) as usize;
                    format!("{}.{} D{},D{},{}", mnemonic, sz, dc, du, self.ea(instr)?)
                }
            }

            // M68851 PMMU
            InstructionMnemonic::POP_000 => {
                let extword = self.get16()?;

                if extword & 0b1110_0001_0000_0000 == 0b0010_0000_0000_0000 {
                    // PFLUSH
                    "PFLUSH".to_string()
                } else if extword == 0b1010_0000_0000_0000 {
                    // PFLUSHR
                    "PFLUSHR".to_string()
                } else if extword & 0b1110_0001_1111_1111 == 0b0100_0000_0000_0000 {
                    // PMOVE (format 1)
                    let extword = Pmove1Extword(extword);
                    let preg = match extword.preg() {
                        0b000 => "PTC",
                        0b001 => "PDRP",
                        0b010 => "PSRP",
                        0b011 => "PCRP",
                        0b100 => "PCAL",
                        0b101 => "PVAL",
                        0b110 => "PSCC",
                        0b111 => "PAC",
                        _ => unreachable!(),
                    };
                    if extword.write() {
                        format!("PMOVE {},{}", preg, self.ea(instr)?)
                    } else {
                        format!("PMOVE {},{}", self.ea(instr)?, preg)
                    }
                } else if extword & 0b1110_0001_1110_0011 == 0b0110_0000_0000_0000 {
                    // PMOVE (format 2)
                    "PMOVE2".to_string()
                } else if extword & 0b1110_0011_1111_1111 == 0b0110_0000_0000_0000 {
                    // PMOVE (format 3)
                    "PMOVE3".to_string()
                } else if extword & 0b1110_0000_0000_0000 == 0b1000_0000_0000_0000 {
                    // PTEST
                    let extword = PtestExtword(extword);
                    let fc = if extword.fc() & 0b10000 != 0 {
                        format!("{}", extword.fc() & 0b1111)
                    } else if extword.fc() & 0b11000 == 0b01000 {
                        format!("D{}", extword.fc() & 0b111)
                    } else if extword.fc() & 0b11111 == 0 {
                        "SFC".to_string()
                    } else if extword.fc() & 0b11111 == 1 {
                        "DFC".to_string()
                    } else {
                        "<invalid>".to_string()
                    };
                    format!(
                        "PTEST {},{},#{}{}",
                        fc,
                        self.ea(instr)?,
                        extword.level(),
                        if extword.a_set() {
                            format!(",A{}", extword.an())
                        } else {
                            "".to_string()
                        }
                    )
                } else {
                    format!("P??? {:04X}", extword)
                }
            }
        };

        Ok(())
    }
}

impl Iterator for Disassembler<'_> {
    type Item = DisassemblyEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let op_msb = self.iter.next()?;
        let op_lsb = self.iter.next()?;
        let opcode = ((op_msb as u16) << 8) | (op_lsb as u16);
        self.out.raw.push(op_msb);
        self.out.raw.push(op_lsb);

        let instr = Instruction::try_decode(CpuM68kType::MAX, opcode);

        if let Ok(i) = instr {
            self.do_instr(&i).ok()?;
        } else {
            self.out.str = format!("Cannot decode {:04X} / {:016b}", opcode, opcode);
        }
        self.addr = self.addr.wrapping_add(self.out.raw.len() as Address);

        let out = self.out.clone();
        self.out = DisassemblyEntry {
            addr: self.addr,
            raw: ArrayVec::new(),
            str: String::default(),
        };

        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dasm(b: &[u8]) -> String {
        let mut iter = b.into_iter().copied();
        let mut disasm = Disassembler::from(&mut iter, 0);
        let disasm_entry = disasm.next();
        let result = disasm_entry.unwrap().str;

        // Ensure entire instruction is consumed
        assert!(disasm.next().is_none());

        result
    }

    #[test]
    fn jsr() {
        assert_eq!(dasm(&[0x4E, 0xBA, 0x01, 0xFA]), "JSR $000001FC");
    }

    #[test]
    fn m68020_indirect_index_base() {
        assert_eq!(
            dasm(&[0x24, 0x70, 0x25, 0xA0, 0x12, 0x34]),
            "MOVEA.l ($1234,-,D2.w*4),A2"
        );
        assert_eq!(
            dasm(&[0x24, 0x70, 0b10100101, 0b10100000, 0x12, 0x34]),
            "MOVEA.l ($1234,-,A2.w*4),A2"
        );
        assert_eq!(
            dasm(&[0x24, 0x70, 0b00100101, 0b10100000, 0x12, 0x34]),
            "MOVEA.l ($1234,-,D2.w*4),A2"
        );
        assert_eq!(
            dasm(&[0x24, 0x70, 0b00100001, 0b10100000, 0x12, 0x34]),
            "MOVEA.l ($1234,-,D2.w*1),A2"
        );
        assert_eq!(
            dasm(&[0x24, 0x70, 0b00100001, 0b10010000]),
            "MOVEA.l ($0000,-,D2.w*1),A2"
        );
        assert_eq!(
            dasm(&[0x24, 0x74, 0x31, 0x10]),
            "MOVEA.l ($0000,A4,D3.w*1),A2"
        );
    }

    #[test]
    fn cmpi_ea() {
        assert_eq!(dasm(&[0x0C, 0x11, 0x00, 0xA8]), "CMPI.b #$A8,(A1)");
    }
}
