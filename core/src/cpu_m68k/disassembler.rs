use anyhow::{bail, Context, Result};
use arrayvec::ArrayVec;
use either::Either;
use itertools::Itertools;

use std::fmt::Write;

use crate::{
    bus::Address,
    cpu_m68k::instruction::{IndexSize, Xn},
    types::Byte,
};

use super::{
    instruction::{AddressingMode, Direction, Instruction, InstructionMnemonic, InstructionSize},
    CpuM68kType,
};

#[derive(Clone)]
pub struct DisassemblyEntry {
    pub addr: Address,
    pub raw: ArrayVec<u8, 12>,
    pub str: String,
}

impl DisassemblyEntry {
    pub fn raw_as_string(&self) -> String {
        self.raw.iter().fold(String::new(), |mut output, b| {
            let _ = write!(output, "{b:02X}");
            output
        })
    }
}

impl std::fmt::Display for DisassemblyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            ":{:06X} {:<16} {}",
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

    fn ea(&mut self, instr: &Instruction) -> Result<String> {
        self.ea_with(instr, instr.get_addr_mode()?, instr.get_op2())
    }

    fn ea_left(&mut self, instr: &Instruction) -> Result<String> {
        self.ea_with(instr, instr.get_addr_mode_left()?, instr.get_op1())
    }

    fn ea_with(&mut self, instr: &Instruction, mode: AddressingMode, op: usize) -> Result<String> {
        instr.clear_extword();
        Ok(match mode {
            AddressingMode::Immediate => match instr.get_size() {
                InstructionSize::Byte => format!("#${:02X}", self.get16()?),
                InstructionSize::Word => format!("#${:04X}", self.get16()?),
                InstructionSize::Long => format!("#${:08X}", self.get32()?),
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
                    "${:06X}",
                    self.addr.wrapping_add_signed(instr.get_displacement()? + 2)
                )
            }
            AddressingMode::IndirectIndex => {
                instr.fetch_extword(|| self.get16())?;

                let extword = instr.get_extword()?;
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
            _ => format!("{:?}", mode),
        })
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
                } else {
                    instr.get_bxx_displacement()
                };
                format!(
                    "B{}.{} {:06X}",
                    if instr.mnemonic == InstructionMnemonic::Bcc {
                        Self::CC[instr.get_cc()]
                    } else {
                        "SR"
                    },
                    if instr.get_bxx_displacement() == 0 {
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
                    "DB{} D{},${:06X}",
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
                    "{}.{} {},D{}",
                    mnemonic,
                    sz,
                    match instr.get_size() {
                        InstructionSize::Long => format!("#${:08X}", self.get32()?),
                        InstructionSize::Word => format!("#${:04X}", self.get16()?),
                        InstructionSize::Byte => format!("#${:02X}", self.get16()?),
                        _ => unreachable!(),
                    },
                    instr.get_op2()
                )
            }

            InstructionMnemonic::LEA => {
                format!("{} {},A{}", mnemonic, self.ea(instr)?, instr.get_op2())
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

            InstructionMnemonic::MOVEtoCCR => format!("MOVE.w CCR,{}", self.ea(instr)?),
            InstructionMnemonic::MOVEtoSR => format!("MOVE.w SR,{}", self.ea(instr)?),
            InstructionMnemonic::MOVEfromSR => format!("MOVE.w {},SR", self.ea(instr)?),
            InstructionMnemonic::MOVEfromUSP => format!("MOVE.l A{},USP", instr.get_op2()),
            InstructionMnemonic::MOVEtoUSP => format!("MOVE.l USP,A{}", instr.get_op2()),
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

            InstructionMnemonic::EXT_l | InstructionMnemonic::EXT_w => {
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

            InstructionMnemonic::CHK => {
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

            InstructionMnemonic::LINK => {
                instr.fetch_extword(|| self.get16())?;
                format!(
                    "{} A{},#{}",
                    mnemonic,
                    instr.get_op2(),
                    instr.get_displacement()?
                )
            }

            InstructionMnemonic::UNLINK => format!("{} A{}", mnemonic, instr.get_op2()),

            InstructionMnemonic::PEA | InstructionMnemonic::TAS => {
                format!("{} {}", mnemonic, self.ea(instr)?)
            }

            InstructionMnemonic::SWAP => format!("{} D{}", mnemonic, instr.get_op2()),

            InstructionMnemonic::MULU_w | InstructionMnemonic::MULS_w => {
                format!("{} {},D{}", mnemonic, self.ea(instr)?, instr.get_op1())
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
        disasm_entry.unwrap().str
    }

    #[test]
    fn jsr() {
        assert_eq!(dasm(&[0x4E, 0xBA, 0x01, 0xFA]), "JSR $0001FC");
    }
}
