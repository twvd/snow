use arrayvec::ArrayVec;
use either::Either;
use itertools::Itertools;

use std::fmt::Write;

use crate::bus::Address;

use super::instruction::{AddressingMode, Instruction, InstructionMnemonic};

pub struct DisassemblyEntry {
    addr: Address,
    raw: ArrayVec<u8, 8>,
    str: String,
}

impl std::fmt::Display for DisassemblyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = self.raw.iter().fold(String::new(), |mut output, b| {
            let _ = write!(output, "{b:02X}");
            output
        });
        write!(f, ":{:06X} {:<16} {}", self.addr, bytes, self.str)
    }
}

pub struct Disassembler<'a> {
    addr: Address,
    iter: &'a mut dyn Iterator<Item = u8>,
}

impl<'a> Disassembler<'a> {
    const CC: &'static [&'static str; 16] = &[
        "RA", "??", "HI", "LS", "CC", "CS", "NE", "EQ", "VC", "VS", "PL", "MI", "GE", "LT", "GT",
        "LE",
    ];
    const MOVEM_REGS: &'static [&'static str; 16] = &[
        "A7", "A6", "A5", "A4", "A3", "A2", "A1", "A0", "D7", "D6", "D5", "D4", "D3", "D2", "D1",
        "D0",
    ];

    pub fn from(iter: &'a mut dyn Iterator<Item = u8>, addr: Address) -> Self {
        Self { addr, iter }
    }

    fn ea(&mut self, instr: &Instruction, out: &mut DisassemblyEntry) -> Option<String> {
        let mut get8 = || {
            let data = self.iter.next()?;
            out.raw.push(data);
            Some(data)
        };
        let mut get16 = || {
            let msb = get8()?;
            let lsb = get8()?;
            Some(((msb as u16) << 8) | (lsb as u16))
        };
        let mut get32 = || {
            let upper = get16()?;
            let lower = get16()?;
            Some(((upper as u32) << 16) | (lower as u32))
        };

        Some(match instr.get_addr_mode().ok()? {
            AddressingMode::AbsoluteShort => format!("(${:04X})", get16()?),
            AddressingMode::AbsoluteLong => format!("(${:08X})", get32()?),
            _ => format!("{:?}", instr.get_addr_mode().ok()?),
        })
    }

    fn do_instr(&mut self, instr: &Instruction, out: &mut DisassemblyEntry) -> Option<()> {
        let mnemonic = instr
            .mnemonic
            .to_string()
            .chars()
            .take_while(|&c| c != '_')
            .collect::<String>();
        let sz = instr.mnemonic.to_string().chars().last()?;

        let mut get8 = || {
            let data = self.iter.next()?;
            out.raw.push(data);
            Some(data)
        };
        let mut get16 = || {
            let msb = get8()?;
            let lsb = get8()?;
            Some(((msb as u16) << 8) | (lsb as u16))
        };
        let mut get32 = || {
            let upper = get16()?;
            let lower = get16()?;
            Some(((upper as u32) << 16) | (lower as u32))
        };

        out.str = match instr.mnemonic {
            InstructionMnemonic::ILLEGAL
            | InstructionMnemonic::NOP
            | InstructionMnemonic::STOP
            | InstructionMnemonic::RESET
            | InstructionMnemonic::RTE
            | InstructionMnemonic::RTR
            | InstructionMnemonic::RTS
            | InstructionMnemonic::TRAPV => mnemonic,

            InstructionMnemonic::Bcc => {
                let displacement = if instr.get_bxx_displacement() == 0 {
                    get16()? as i16 as i32
                } else {
                    instr.get_bxx_displacement()
                };
                format!(
                    "B{} {:06X}",
                    Self::CC[instr.get_cc()],
                    self.addr.wrapping_add_signed(displacement + 2)
                )
            }

            InstructionMnemonic::MOVEM_reg_w | InstructionMnemonic::MOVEM_reg_l => {
                let mask = get16()?;

                let regs = if instr.get_addr_mode().ok()? != AddressingMode::IndirectPreDec {
                    Either::Left(Self::MOVEM_REGS.iter().rev())
                } else {
                    Either::Right(Self::MOVEM_REGS.iter())
                };

                format!(
                    "MOVEM.{} {}, [{}]",
                    sz,
                    self.ea(instr, out)?,
                    regs.enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, r)| r)
                        .join("/")
                )
            }

            InstructionMnemonic::CMP_l
            | InstructionMnemonic::CMP_w
            | InstructionMnemonic::CMP_b => format!(
                "{}.{} {},D{}",
                mnemonic,
                sz,
                self.ea(instr, out)?,
                instr.get_op2()
            ),

            InstructionMnemonic::CMPI_l
            | InstructionMnemonic::CMPI_w
            | InstructionMnemonic::CMPI_b => {
                format!(
                    "{}.{} {},D{}",
                    mnemonic,
                    sz,
                    match sz {
                        'l' => format!("#{:08X}", get32()?),
                        'w' => format!("#{:04X}", get16()?),
                        'b' => format!("#{:02X}", get8()?),
                        _ => unreachable!(),
                    },
                    instr.get_op2()
                )
            }

            InstructionMnemonic::ABCD
            | InstructionMnemonic::MOVEM_mem_w
            | InstructionMnemonic::MOVEM_mem_l
            | InstructionMnemonic::Scc
            | InstructionMnemonic::DBcc
            | InstructionMnemonic::NBCD
            | InstructionMnemonic::SBCD
            | InstructionMnemonic::ADD_l
            | InstructionMnemonic::ADD_w
            | InstructionMnemonic::ADD_b
            | InstructionMnemonic::ADDA_l
            | InstructionMnemonic::ADDA_w
            | InstructionMnemonic::ADDI_l
            | InstructionMnemonic::ADDI_w
            | InstructionMnemonic::ADDI_b
            | InstructionMnemonic::ADDQ_l
            | InstructionMnemonic::ADDQ_w
            | InstructionMnemonic::ADDQ_b
            | InstructionMnemonic::ADDX_l
            | InstructionMnemonic::ADDX_w
            | InstructionMnemonic::ADDX_b
            | InstructionMnemonic::AND_l
            | InstructionMnemonic::AND_w
            | InstructionMnemonic::AND_b
            | InstructionMnemonic::ANDI_l
            | InstructionMnemonic::ANDI_w
            | InstructionMnemonic::ANDI_b
            | InstructionMnemonic::ANDI_ccr
            | InstructionMnemonic::ANDI_sr
            | InstructionMnemonic::ASL_ea
            | InstructionMnemonic::ASL_b
            | InstructionMnemonic::ASL_w
            | InstructionMnemonic::ASL_l
            | InstructionMnemonic::ASR_b
            | InstructionMnemonic::ASR_w
            | InstructionMnemonic::ASR_l
            | InstructionMnemonic::ASR_ea
            | InstructionMnemonic::BCHG_dn
            | InstructionMnemonic::BCLR_dn
            | InstructionMnemonic::BSET_dn
            | InstructionMnemonic::BTST_dn
            | InstructionMnemonic::BCHG_imm
            | InstructionMnemonic::BCLR_imm
            | InstructionMnemonic::BSET_imm
            | InstructionMnemonic::BTST_imm
            | InstructionMnemonic::BSR
            | InstructionMnemonic::CHK
            | InstructionMnemonic::CLR_l
            | InstructionMnemonic::CLR_w
            | InstructionMnemonic::CLR_b
            | InstructionMnemonic::CMPA_l
            | InstructionMnemonic::CMPA_w
            | InstructionMnemonic::CMPM_l
            | InstructionMnemonic::CMPM_w
            | InstructionMnemonic::CMPM_b
            | InstructionMnemonic::DIVS_w
            | InstructionMnemonic::DIVU_w
            | InstructionMnemonic::EOR_l
            | InstructionMnemonic::EOR_w
            | InstructionMnemonic::EOR_b
            | InstructionMnemonic::EORI_l
            | InstructionMnemonic::EORI_w
            | InstructionMnemonic::EORI_b
            | InstructionMnemonic::EORI_ccr
            | InstructionMnemonic::EORI_sr
            | InstructionMnemonic::EXG
            | InstructionMnemonic::EXT_l
            | InstructionMnemonic::EXT_w
            | InstructionMnemonic::JMP
            | InstructionMnemonic::JSR
            | InstructionMnemonic::LSL_ea
            | InstructionMnemonic::LSL_b
            | InstructionMnemonic::LSL_w
            | InstructionMnemonic::LSL_l
            | InstructionMnemonic::LSR_b
            | InstructionMnemonic::LSR_w
            | InstructionMnemonic::LSR_l
            | InstructionMnemonic::LSR_ea
            | InstructionMnemonic::OR_l
            | InstructionMnemonic::OR_w
            | InstructionMnemonic::OR_b
            | InstructionMnemonic::ORI_l
            | InstructionMnemonic::ORI_w
            | InstructionMnemonic::ORI_b
            | InstructionMnemonic::ORI_ccr
            | InstructionMnemonic::ORI_sr
            | InstructionMnemonic::LEA
            | InstructionMnemonic::LINEA
            | InstructionMnemonic::LINEF
            | InstructionMnemonic::LINK
            | InstructionMnemonic::UNLINK
            | InstructionMnemonic::MOVE_w
            | InstructionMnemonic::MOVE_l
            | InstructionMnemonic::MOVE_b
            | InstructionMnemonic::MOVEA_w
            | InstructionMnemonic::MOVEA_l
            | InstructionMnemonic::MOVEP_w
            | InstructionMnemonic::MOVEP_l
            | InstructionMnemonic::MOVEfromSR
            | InstructionMnemonic::MOVEfromUSP
            | InstructionMnemonic::MOVEtoCCR
            | InstructionMnemonic::MOVEtoSR
            | InstructionMnemonic::MOVEtoUSP
            | InstructionMnemonic::MOVEQ
            | InstructionMnemonic::MULU_w
            | InstructionMnemonic::MULS_w
            | InstructionMnemonic::NEG_l
            | InstructionMnemonic::NEG_w
            | InstructionMnemonic::NEG_b
            | InstructionMnemonic::NEGX_l
            | InstructionMnemonic::NEGX_w
            | InstructionMnemonic::NEGX_b
            | InstructionMnemonic::NOT_l
            | InstructionMnemonic::NOT_w
            | InstructionMnemonic::NOT_b
            | InstructionMnemonic::PEA
            | InstructionMnemonic::ROXL_ea
            | InstructionMnemonic::ROXL_b
            | InstructionMnemonic::ROXL_w
            | InstructionMnemonic::ROXL_l
            | InstructionMnemonic::ROXR_b
            | InstructionMnemonic::ROXR_w
            | InstructionMnemonic::ROXR_l
            | InstructionMnemonic::ROXR_ea
            | InstructionMnemonic::ROL_ea
            | InstructionMnemonic::ROL_b
            | InstructionMnemonic::ROL_w
            | InstructionMnemonic::ROL_l
            | InstructionMnemonic::ROR_b
            | InstructionMnemonic::ROR_w
            | InstructionMnemonic::ROR_l
            | InstructionMnemonic::ROR_ea
            | InstructionMnemonic::SUB_l
            | InstructionMnemonic::SUB_w
            | InstructionMnemonic::SUB_b
            | InstructionMnemonic::SUBA_l
            | InstructionMnemonic::SUBA_w
            | InstructionMnemonic::SUBI_l
            | InstructionMnemonic::SUBI_w
            | InstructionMnemonic::SUBI_b
            | InstructionMnemonic::SUBQ_l
            | InstructionMnemonic::SUBQ_w
            | InstructionMnemonic::SUBQ_b
            | InstructionMnemonic::SUBX_l
            | InstructionMnemonic::SUBX_w
            | InstructionMnemonic::SUBX_b
            | InstructionMnemonic::SWAP
            | InstructionMnemonic::TAS
            | InstructionMnemonic::TRAP
            | InstructionMnemonic::TST_l
            | InstructionMnemonic::TST_w
            | InstructionMnemonic::TST_b => format!("TODO {}", instr.mnemonic),
        };

        Some(())
    }
}

impl<'a> Iterator for Disassembler<'a> {
    type Item = DisassemblyEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let mut result = Self::Item {
            addr: self.addr,
            raw: ArrayVec::new(),
            str: String::default(),
        };

        let op_msb = self.iter.next()?;
        let op_lsb = self.iter.next()?;
        let opcode = ((op_msb as u16) << 8) | (op_lsb as u16);
        result.raw.push(op_msb);
        result.raw.push(op_lsb);

        let instr = Instruction::try_decode(opcode);

        if let Ok(i) = instr {
            self.do_instr(&i, &mut result)?;
        } else {
            result.str = format!("Cannot decode {:04X} / {:016b}", opcode, opcode);
        }
        self.addr = self.addr.wrapping_add(result.raw.len() as Address);

        Some(result)
    }
}
