use std::collections::VecDeque;

use anyhow::{bail, Result};
use num_traits::FromBytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};
use crate::util::TemporalOrder;

use super::instruction::{
    AddressingMode, Direction, IndexSize, Instruction, InstructionMnemonic, Xn,
};
use super::regs::{RegisterFile, RegisterSR};
use super::{Byte, CpuSized, Long, Word};

/// Access error details
#[derive(Debug, Clone, Copy)]
struct AccessError {
    #[allow(dead_code)]
    function_code: u8,
    read: bool,
    #[allow(dead_code)]
    instruction: bool,
    address: Address,
    ir: Word,
}

/// CPU error type to cascade exceptions down
#[derive(Error, Debug)]
enum CpuError {
    /// Access error exception (unaligned address on Word/Long access)
    #[error("Access error exception")]
    AccessError(AccessError),
}

/// M68000 exception groups
enum ExceptionGroup {
    Group0,
    Group1,
    Group2,
}

// Exception vectors
/// Address error exception vector
const VECTOR_ACCESS_ERROR: Address = 0x00000C;
/// Illegal instruction exception vector
const VECTOR_ILLEGAL: Address = 0x000010;
/// Division by zero exception vector
const VECTOR_DIV_ZERO: Address = 0x000014;
/// Privilege violation exception vector
const VECTOR_PRIVILEGE_VIOLATION: Address = 0x000020;
/// Trap exception vector offset (15 vectors)
const VECTOR_TRAP_OFFSET: Address = 0x000080;

/// Motorola 680x0
#[derive(Serialize, Deserialize)]
pub struct CpuM68k<TBus: Bus<Address, u8>> {
    /// Exception occured this step
    pub step_exception: bool,
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
    pub prefetch: VecDeque<u16>,

    step_ea_addr: Option<Address>,
    step_ea_load: Option<(usize, Address)>,
}

impl<TBus> CpuM68k<TBus>
where
    TBus: Bus<Address, u8>,
{
    pub fn new(bus: TBus) -> Self {
        Self {
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            prefetch: VecDeque::with_capacity(3),
            step_ea_addr: None,
            step_exception: false,
            step_ea_load: None,
        }
    }

    fn prefetch_pump(&mut self) -> Result<()> {
        if self.prefetch.len() >= 2 {
            return Ok(());
        }
        self.prefetch_pump_force()
    }

    fn prefetch_pump_force(&mut self) -> Result<()> {
        let fetch_addr = ((self.regs.pc & !1) + 4) & ADDRESS_MASK;
        let new_item = self.read_ticks::<Word>(fetch_addr)?;
        self.prefetch.push_back(new_item);
        self.regs.pc = (self.regs.pc + 2) & ADDRESS_MASK;
        Ok(())
    }

    /// Re-fills the prefetch queue
    fn prefetch_refill(&mut self) -> Result<()> {
        while self.prefetch.len() < 2 {
            self.prefetch_pump()?;
        }
        Ok(())
    }

    /// Fetches a 16-bit value, through the prefetch queue
    fn fetch_pump(&mut self) -> Result<Word> {
        self.prefetch_pump_force()?;
        let v = self.prefetch.pop_front().unwrap();
        Ok(v)
    }

    /// Fetches a 16-bit value from prefetch queue
    fn fetch(&mut self) -> Result<Word> {
        if self.prefetch.len() == 0 {
            self.prefetch_pump()?;
        }
        Ok(self.prefetch.pop_front().unwrap())
    }

    /// Executes a single CPU step.
    pub fn step(&mut self) -> Result<()> {
        debug_assert_eq!(self.prefetch.len(), 2);

        self.step_ea_addr = None;
        self.step_exception = false;

        let instr = Instruction::try_decode(|| self.fetch())?;
        // TODO decoded instruction cache

        match self.execute_instruction(&instr) {
            Ok(()) => {
                // Assert ea_commit() was called
                debug_assert!(self.step_ea_load.is_none());
            }
            Err(e) => match e.downcast_ref() {
                Some(CpuError::AccessError(ae)) => {
                    let mut details = *ae;
                    details.ir = instr.data;
                    self.raise_exception(
                        ExceptionGroup::Group0,
                        VECTOR_ACCESS_ERROR,
                        Some(details),
                    )?
                }
                None => return Err(e),
            },
        };

        self.prefetch_refill()?;
        Ok(())
    }

    /// Advances by the given amount of cycles
    fn advance_cycles(&mut self, ticks: Ticks) -> Result<()> {
        for _ in 0..ticks {
            self.cycles += 1;
            self.bus.tick(1)?;
        }
        Ok(())
    }

    /// Checks if an access needs to fail and raise bus error on alignment errors
    fn verify_access<T: CpuSized>(&mut self, addr: Address, read: bool) -> Result<()> {
        if std::mem::size_of::<T>() >= 2 && (addr & 1) != 0 {
            // Unaligned access
            eprintln!("Unaligned access: address {:08X}", addr);
            bail!(CpuError::AccessError(AccessError {
                function_code: 0,
                ir: 0,

                // TODO instruction bit
                instruction: false,
                read,
                address: addr
            }));
        }
        Ok(())
    }

    /// Reads a value from the bus and spends ticks.
    fn read_ticks<T: CpuSized>(&mut self, oaddr: Address) -> Result<T> {
        let len = std::mem::size_of::<T>();
        let mut result: T = T::zero();
        let addr = if len > 1 { oaddr & !1 } else { oaddr };

        // Below converts from BE -> LE on the fly
        for a in 0..len {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b: T = self.bus.read(byte_addr).into();
            result = result.wrapping_shl(8) | b;

            self.advance_cycles(2)?;

            if a == 1 {
                // Address errors occur AFTER the first Word was accessed and not at all if
                // it is a byte access, so this is the perfect time to check.
                self.verify_access::<T>(oaddr, true)?;
            }
        }

        if len == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(result)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    fn write_ticks<T: CpuSized>(&mut self, addr: Address, value: T) -> Result<()> {
        self.write_ticks_order(addr, value, TemporalOrder::LowToHigh)
    }

    fn write_ticks_order<T: CpuSized>(
        &mut self,
        oaddr: Address,
        value: T,
        order: TemporalOrder,
    ) -> Result<()> {
        let addr = if std::mem::size_of::<T>() > 1 {
            oaddr & !1
        } else {
            oaddr
        };

        match order {
            TemporalOrder::LowToHigh => {
                let mut val: Long = value.to_be().into();
                for a in 0..std::mem::size_of::<T>() {
                    let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
                    let b = val as u8;
                    val = val >> 8;

                    self.bus.write(byte_addr, b);
                    self.advance_cycles(2)?;
                    if a == 1 {
                        // Address errors occur AFTER the first Word was accessed and not at all if
                        // it is a byte access, so this is the perfect time to check.
                        self.verify_access::<T>(oaddr, true)?;
                    }
                }
            }
            TemporalOrder::HighToLow => {
                let mut val: Long = value.into();
                for a in (0..std::mem::size_of::<T>()).rev() {
                    let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
                    let b = val as u8;
                    val = val >> 8;

                    self.bus.write(byte_addr, b);
                    self.advance_cycles(2)?;

                    if a == 1 {
                        // Address errors occur AFTER the first Word was accessed and not at all if
                        // it is a byte access, so this is the perfect time to check.
                        self.verify_access::<T>(oaddr, true)?;
                    }
                }
            }
        }

        if std::mem::size_of::<T>() == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// Sets the program counter and flushes the prefetch queue
    fn set_pc(&mut self, pc: Address) -> Result<()> {
        self.prefetch.clear();
        self.regs.pc = pc.wrapping_sub(4) & ADDRESS_MASK;
        Ok(())
    }

    /// Raises a CPU exception in supervisor mode.
    fn raise_exception(
        &mut self,
        group: ExceptionGroup,
        vector: Address,
        details: Option<AccessError>,
    ) -> Result<()> {
        self.step_exception = true;

        self.advance_cycles(4)?; // idle

        // Resume in supervisor mode
        self.regs.sr.set_supervisor(true);
        self.regs.sr.set_trace(false);

        // Write exception stack frame
        match group {
            ExceptionGroup::Group0 => {
                self.advance_cycles(4)?; // idle
                let details = details.expect("Address error details not passed");
                eprintln!(
                    "Access error: read = {:?}, address = {:08X} PC = {:06X}",
                    details.read, details.address, self.regs.pc
                );

                self.regs.ssp = self.regs.ssp.wrapping_sub(14);
                self.write_ticks(self.regs.ssp.wrapping_add(12), self.regs.pc as u16)?;
                self.write_ticks(self.regs.ssp.wrapping_add(8), self.regs.sr.sr())?;
                self.write_ticks(self.regs.ssp.wrapping_add(10), (self.regs.pc >> 16) as u16)?;
                self.write_ticks(self.regs.ssp.wrapping_add(6), details.ir)?;
                self.write_ticks(self.regs.ssp.wrapping_add(4), details.address as u16)?;
                // Function code (3), I/N (1), R/W (1)
                // TODO I/N, function code..
                self.write_ticks(
                    self.regs.ssp.wrapping_add(0),
                    if details.read { 1_u16 << 4 } else { 0_u16 },
                )?;
                self.write_ticks(
                    self.regs.ssp.wrapping_add(2),
                    (details.address >> 16) as u16,
                )?;
            }
            ExceptionGroup::Group1 | ExceptionGroup::Group2 => {
                // Advance PC beyond the current instruction to
                // have the right offset for the stack frame.
                // The current prefetch queue length provides an indication
                // of the current instruction length.
                self.regs.pc = self
                    .regs
                    .pc
                    .wrapping_add((2 - (self.prefetch.len() as u32)) * 2);

                self.regs.ssp = self.regs.ssp.wrapping_sub(6);
                self.write_ticks(self.regs.ssp.wrapping_add(4), self.regs.pc as u16)?;
                self.write_ticks(self.regs.ssp.wrapping_add(0), self.regs.sr.sr())?;
                self.write_ticks(self.regs.ssp.wrapping_add(2), (self.regs.pc >> 16) as u16)?;
            }
        }

        let new_pc = self.read_ticks::<Long>(vector)?.into();
        self.set_pc(new_pc)?;
        self.prefetch_pump()?;
        self.advance_cycles(2)?; // 2x idle
        self.prefetch_pump()?;

        Ok(())
    }

    /// Executes a previously decoded instruction.
    fn execute_instruction(&mut self, instr: &Instruction) -> Result<()> {
        match instr.mnemonic {
            InstructionMnemonic::AND_l => self.op_bitwise::<Long>(&instr, |a, b| a & b),
            InstructionMnemonic::AND_w => self.op_bitwise::<Word>(&instr, |a, b| a & b),
            InstructionMnemonic::AND_b => self.op_bitwise::<Byte>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_l => self.op_bitwise_immediate::<Long>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_w => self.op_bitwise_immediate::<Word>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_b => self.op_bitwise_immediate::<Byte>(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_ccr => self.op_bitwise_ccr(&instr, |a, b| a & b),
            InstructionMnemonic::ANDI_sr => self.op_bitwise_sr(&instr, |a, b| a & b),
            InstructionMnemonic::EOR_l => self.op_bitwise::<Long>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EOR_w => self.op_bitwise::<Word>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EOR_b => self.op_bitwise::<Byte>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_l => self.op_bitwise_immediate::<Long>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_w => self.op_bitwise_immediate::<Word>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_b => self.op_bitwise_immediate::<Byte>(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_ccr => self.op_bitwise_ccr(&instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_sr => self.op_bitwise_sr(&instr, |a, b| a ^ b),
            InstructionMnemonic::OR_l => self.op_bitwise::<Long>(&instr, |a, b| a | b),
            InstructionMnemonic::OR_w => self.op_bitwise::<Word>(&instr, |a, b| a | b),
            InstructionMnemonic::OR_b => self.op_bitwise::<Byte>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_l => self.op_bitwise_immediate::<Long>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_w => self.op_bitwise_immediate::<Word>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_b => self.op_bitwise_immediate::<Byte>(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_ccr => self.op_bitwise_ccr(&instr, |a, b| a | b),
            InstructionMnemonic::ORI_sr => self.op_bitwise_sr(&instr, |a, b| a | b),
            InstructionMnemonic::SUB_l => self.op_alu::<Long>(&instr, Self::alu_sub),
            InstructionMnemonic::SUB_w => self.op_alu::<Word>(&instr, Self::alu_sub),
            InstructionMnemonic::SUB_b => self.op_alu::<Byte>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBA_l => self.op_alu_a::<Long>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBA_w => self.op_alu_a::<Word>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBI_l => self.op_alu_immediate::<Long>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBI_w => self.op_alu_immediate::<Word>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBI_b => self.op_alu_immediate::<Byte>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBQ_l => self.op_alu_quick::<Long>(&instr, Self::alu_sub),
            InstructionMnemonic::SUBQ_w => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    // A word operation on an address register affects the entire 32-bit address.
                    self.op_alu_quick::<Long>(&instr, Self::alu_sub)
                } else {
                    self.op_alu_quick::<Word>(&instr, Self::alu_sub)
                }
            }
            InstructionMnemonic::SUBQ_b => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    panic!("TODO SUB.b Q, An is illegal!");
                }
                self.op_alu_quick::<Byte>(&instr, Self::alu_sub)
            }
            InstructionMnemonic::SUBX_l => self.op_alu_x::<Long>(&instr, Self::alu_sub_x),
            InstructionMnemonic::SUBX_w => self.op_alu_x::<Word>(&instr, Self::alu_sub_x),
            InstructionMnemonic::SUBX_b => self.op_alu_x::<Byte>(&instr, Self::alu_sub_x),
            InstructionMnemonic::ADD_l => self.op_alu::<Long>(&instr, Self::alu_add),
            InstructionMnemonic::ADD_w => self.op_alu::<Word>(&instr, Self::alu_add),
            InstructionMnemonic::ADD_b => self.op_alu::<Byte>(&instr, Self::alu_add),
            InstructionMnemonic::ADDA_l => self.op_alu_a::<Long>(&instr, Self::alu_add),
            InstructionMnemonic::ADDA_w => self.op_alu_a::<Word>(&instr, Self::alu_add),
            InstructionMnemonic::ADDI_l => self.op_alu_immediate::<Long>(&instr, Self::alu_add),
            InstructionMnemonic::ADDI_w => self.op_alu_immediate::<Word>(&instr, Self::alu_add),
            InstructionMnemonic::ADDI_b => self.op_alu_immediate::<Byte>(&instr, Self::alu_add),
            InstructionMnemonic::ADDQ_l => self.op_alu_quick::<Long>(&instr, Self::alu_add),
            InstructionMnemonic::ADDQ_w => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    // A word operation on an address register affects the entire 32-bit address.
                    self.op_alu_quick::<Long>(&instr, Self::alu_add)
                } else {
                    self.op_alu_quick::<Word>(&instr, Self::alu_add)
                }
            }
            InstructionMnemonic::ADDQ_b => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    panic!("TODO ADD.b Q, An is illegal!");
                }
                self.op_alu_quick::<Byte>(&instr, Self::alu_add)
            }
            InstructionMnemonic::ADDX_l => self.op_alu_x::<Long>(&instr, Self::alu_add_x),
            InstructionMnemonic::ADDX_w => self.op_alu_x::<Word>(&instr, Self::alu_add_x),
            InstructionMnemonic::ADDX_b => self.op_alu_x::<Byte>(&instr, Self::alu_add_x),
            InstructionMnemonic::CMP_l => self.op_cmp::<Long>(&instr),
            InstructionMnemonic::CMP_w => self.op_cmp::<Word>(&instr),
            InstructionMnemonic::CMP_b => self.op_cmp::<Byte>(&instr),
            InstructionMnemonic::CMPA_l => self.op_cmp_address::<Long>(&instr),
            InstructionMnemonic::CMPA_w => self.op_cmp_address::<Word>(&instr),
            InstructionMnemonic::CMPI_l => self.op_cmp_immediate::<Long>(&instr),
            InstructionMnemonic::CMPI_w => self.op_cmp_immediate::<Word>(&instr),
            InstructionMnemonic::CMPI_b => self.op_cmp_immediate::<Byte>(&instr),
            InstructionMnemonic::CMPM_l => self.op_cmpm::<Long>(&instr),
            InstructionMnemonic::CMPM_w => self.op_cmpm::<Word>(&instr),
            InstructionMnemonic::CMPM_b => self.op_cmpm::<Byte>(&instr),
            InstructionMnemonic::MULU_w => self.op_mulu(&instr),
            InstructionMnemonic::MULS_w => self.op_muls(&instr),
            InstructionMnemonic::DIVU_w => self.op_divu(&instr),
            InstructionMnemonic::DIVS_w => self.op_divs(&instr),
            InstructionMnemonic::NOP => Ok(()),
            InstructionMnemonic::SWAP => self.op_swap(&instr),
            InstructionMnemonic::TRAP => self.op_trap(&instr),
            InstructionMnemonic::BTST_imm => self.op_bit::<true>(&instr, None),
            InstructionMnemonic::BSET_imm => self.op_bit::<true>(&instr, Some(|v, bit| v | bit)),
            InstructionMnemonic::BCLR_imm => self.op_bit::<true>(&instr, Some(|v, bit| v & !bit)),
            InstructionMnemonic::BCHG_imm => self.op_bit::<true>(&instr, Some(|v, bit| v ^ bit)),
            InstructionMnemonic::BTST_dn => self.op_bit::<false>(&instr, None),
            InstructionMnemonic::BSET_dn => self.op_bit::<false>(&instr, Some(|v, bit| v | bit)),
            InstructionMnemonic::BCLR_dn => self.op_bit::<false>(&instr, Some(|v, bit| v & !bit)),
            InstructionMnemonic::BCHG_dn => self.op_bit::<false>(&instr, Some(|v, bit| v ^ bit)),
            InstructionMnemonic::MOVEP_w => self.op_movep::<2, Word>(&instr),
            InstructionMnemonic::MOVEP_l => self.op_movep::<4, Long>(&instr),
            InstructionMnemonic::MOVEA_l => self.op_movea::<Long>(&instr),
            InstructionMnemonic::MOVEA_w => self.op_movea::<Word>(&instr),
            InstructionMnemonic::MOVE_l => self.op_move::<Long>(&instr),
            InstructionMnemonic::MOVE_w => self.op_move::<Word>(&instr),
            InstructionMnemonic::MOVE_b => self.op_move::<Byte>(&instr),
            InstructionMnemonic::MOVEfromSR => self.op_move_from_sr(&instr),
            InstructionMnemonic::MOVEtoSR => self.op_move_to_sr(&instr),
            InstructionMnemonic::MOVEtoCCR => self.op_move_to_ccr(&instr),
            InstructionMnemonic::MOVEtoUSP => self.op_move_to_usp(&instr),
            InstructionMnemonic::MOVEfromUSP => self.op_move_from_usp(&instr),
            InstructionMnemonic::NEG_l => self.op_alu_zero::<Long>(&instr, Self::alu_sub),
            InstructionMnemonic::NEG_w => self.op_alu_zero::<Word>(&instr, Self::alu_sub),
            InstructionMnemonic::NEG_b => self.op_alu_zero::<Byte>(&instr, Self::alu_sub),
            InstructionMnemonic::NEGX_l => self.op_alu_zero::<Long>(&instr, Self::alu_sub_x),
            InstructionMnemonic::NEGX_w => self.op_alu_zero::<Word>(&instr, Self::alu_sub_x),
            InstructionMnemonic::NEGX_b => self.op_alu_zero::<Byte>(&instr, Self::alu_sub_x),
            InstructionMnemonic::CLR_l => self.op_clr::<Long>(&instr),
            InstructionMnemonic::CLR_w => self.op_clr::<Word>(&instr),
            InstructionMnemonic::CLR_b => self.op_clr::<Byte>(&instr),
            InstructionMnemonic::NOT_l => self.op_not::<Long>(&instr),
            InstructionMnemonic::NOT_w => self.op_not::<Word>(&instr),
            InstructionMnemonic::NOT_b => self.op_not::<Byte>(&instr),
            InstructionMnemonic::EXT_l => self.op_ext::<Long, Word>(&instr),
            InstructionMnemonic::EXT_w => self.op_ext::<Word, Byte>(&instr),
            InstructionMnemonic::SBCD => self.op_sbcd(&instr),
            InstructionMnemonic::NBCD => self.op_nbcd(&instr),
            InstructionMnemonic::ABCD => self.op_abcd(&instr),
            InstructionMnemonic::PEA => self.op_pea(&instr),
            InstructionMnemonic::ILLEGAL => {
                self.raise_exception(ExceptionGroup::Group1, VECTOR_ILLEGAL, None)
            }
            InstructionMnemonic::TAS => self.op_tas(&instr),
            InstructionMnemonic::TST_b => self.op_tst::<Byte>(&instr),
            InstructionMnemonic::TST_w => self.op_tst::<Word>(&instr),
            InstructionMnemonic::TST_l => self.op_tst::<Long>(&instr),
            InstructionMnemonic::LINK => self.op_link(&instr),
            InstructionMnemonic::UNLINK => self.op_unlink(&instr),

            _ => todo!(),
        }
    }

    /// Calculates address from effective addressing mode
    /// Happens once per instruction so e.g. postinc/predec only occur once.
    fn calc_ea_addr<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
    ) -> Result<Address> {
        if let Some(addr) = self.step_ea_addr {
            // Already done this step
            return Ok(addr);
        }

        let read_idx = |s: &Self, (xn, reg): (Xn, usize), size: IndexSize| {
            let v = match xn {
                Xn::Dn => s.regs.read_d(reg),
                Xn::An => s.regs.read_a(reg),
            };
            match size {
                IndexSize::Word => v as u16 as i16 as i32 as u32,
                IndexSize::Long => v,
            }
        };
        let addr = match addrmode {
            AddressingMode::DataRegister => unreachable!(),
            AddressingMode::AddressRegister => unreachable!(),
            AddressingMode::Indirect => self.regs.read_a(ea_in),
            AddressingMode::IndirectPreDec => {
                self.advance_cycles(2)?; // 2x idle
                self.regs
                    .read_a_predec::<Address>(ea_in, std::mem::size_of::<T>())
            }
            AddressingMode::IndirectPostInc => self.regs.read_a(ea_in),
            AddressingMode::IndirectDisplacement => {
                instr.fetch_extword(|| self.fetch_pump())?;
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = instr.get_displacement()?;
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::IndirectIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extword(|| self.fetch_pump())?;

                let extword = instr.get_extword()?;
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                addr.wrapping_add(displacement).wrapping_add(index)
            }
            AddressingMode::PCDisplacement => {
                instr.fetch_extword(|| self.fetch_pump())?;
                let addr = self.regs.pc;
                let displacement = instr.get_displacement()?;
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::PCIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extword(|| self.fetch_pump())?;
                let extword = instr.get_extword()?;
                let pc = self.regs.pc;
                let displacement = extword.brief_get_displacement_signext();
                let index = read_idx(
                    self,
                    extword.brief_get_register(),
                    extword.brief_get_index_size(),
                );
                pc.wrapping_add(displacement).wrapping_add(index)
            }
            AddressingMode::AbsoluteShort => self.fetch_pump()? as i16 as i32 as u32,
            AddressingMode::AbsoluteLong => {
                let h = self.fetch_pump()? as u32;
                let l = self.fetch_pump()? as u32;
                (h << 16) | l
            }
            _ => todo!(),
        };

        self.step_ea_addr = Some(addr);
        Ok(addr)
    }

    fn fetch_immediate<T: CpuSized>(&mut self) -> Result<T> {
        Ok(match std::mem::size_of::<T>() {
            1 | 2 => T::chop(self.fetch_pump()?.into()),
            4 => {
                let h = self.fetch_pump()? as u32;
                let l = self.fetch_pump()? as u32;
                T::chop((h << 16) | l)
            }
            _ => unreachable!(),
        })
    }

    /// Commits a postponed 'held' change to address register.
    fn ea_commit(&mut self) {
        if let Some((reg, val)) = self.step_ea_load {
            // Postponed An write from post-increment mode
            self.regs.write_a(reg, val);
        }
        self.step_ea_load = None;
    }

    /// Reads a value from the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn read_ea<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize) -> Result<T> {
        self.read_ea_with(instr, instr.get_addr_mode()?, ea_in, false)
    }

    /// Reads a value from the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    /// Holds off on postincrement.
    fn read_ea_hold<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize) -> Result<T> {
        self.read_ea_with(instr, instr.get_addr_mode()?, ea_in, true)
    }

    fn read_ea_with<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
        hold: bool,
    ) -> Result<T> {
        let v = match addrmode {
            AddressingMode::DataRegister => self.regs.read_d(ea_in),
            AddressingMode::AddressRegister => self.regs.read_a(ea_in),
            AddressingMode::Immediate => self.fetch_immediate::<T>()?,
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::PCDisplacement
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectPreDec => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectPostInc => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                let inc_addr = if ea_in == 7 {
                    // Minimum of 2 for A7
                    addr.wrapping_add(std::cmp::max(2, std::mem::size_of::<T>() as Address))
                } else {
                    addr.wrapping_add(std::mem::size_of::<T>() as Address)
                };
                if !hold || std::mem::size_of::<T>() <= 2 {
                    self.regs.write_a::<Address>(ea_in, inc_addr);
                } else {
                    self.step_ea_load = Some((ea_in, inc_addr));
                }
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectIndex | AddressingMode::PCIndex => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                self.read_ticks(addr)?
            }
        };

        Ok(v)
    }

    /// Writes a value to the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn write_ea<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize, value: T) -> Result<()> {
        self.write_ea_with(
            instr,
            instr.get_addr_mode()?,
            ea_in,
            value,
            TemporalOrder::HighToLow,
            false,
        )
    }

    /// Writes a value to the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn write_ea_hold<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        ea_in: usize,
        value: T,
    ) -> Result<()> {
        self.write_ea_with(
            instr,
            instr.get_addr_mode()?,
            ea_in,
            value,
            TemporalOrder::HighToLow,
            true,
        )
    }

    fn write_ea_with<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        addrmode: AddressingMode,
        ea_in: usize,
        value: T,
        order: TemporalOrder,
        hold: bool,
    ) -> Result<()> {
        match addrmode {
            AddressingMode::DataRegister => Ok(self.regs.write_d(ea_in, value)),
            AddressingMode::AddressRegister => Ok(self.regs.write_a(ea_in, value)),
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectIndex
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                self.write_ticks_order(addr, value, order)
            }
            AddressingMode::IndirectPreDec => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                self.write_ticks_order(addr, value, order)
            }
            AddressingMode::IndirectPostInc => {
                let addr = self.calc_ea_addr::<T>(instr, addrmode, ea_in)?;
                let inc_addr = if ea_in == 7 {
                    // Minimum of 2 for A7
                    addr.wrapping_add(std::cmp::max(2, std::mem::size_of::<T>() as Address))
                } else {
                    addr.wrapping_add(std::mem::size_of::<T>() as Address)
                };
                if !hold {
                    self.regs.write_a::<Address>(ea_in, inc_addr);
                } else {
                    self.step_ea_load = Some((ea_in, inc_addr));
                }
                self.write_ticks_order(addr, value, order)
            }
            _ => todo!(),
        }
    }

    /// SWAP
    fn op_swap(&mut self, instr: &Instruction) -> Result<()> {
        let v: Long = self.regs.read_d(instr.get_op2());
        let result = (v >> 16) | (v << 16);

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & (1 << 31) != 0);
        self.regs.sr.set_z(result == 0);
        self.regs.write_d(instr.get_op2(), result);

        Ok(())
    }

    /// AND/OR/EOR
    fn op_bitwise<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T) -> T,
    ) -> Result<()> {
        let left: T = self.regs.read_d(instr.get_op1());
        let right: T = self.read_ea_hold(instr, instr.get_op2())?;
        self.ea_commit();
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let result = calcfn(a, b);

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs
            .sr
            .set_n(result.reverse_bits() & T::one() != T::zero());
        self.regs.sr.set_z(result == T::zero());

        self.prefetch_pump()?;
        match instr.get_direction() {
            Direction::Right => self.regs.write_d(instr.get_op1(), result),
            Direction::Left => self.write_ea(instr, instr.get_op2(), result)?,
        }

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => {
                self.advance_cycles(4)?
            }

            (_, Direction::Right, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// ANDI/ORI/EORI
    fn op_bitwise_immediate<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T) -> T,
    ) -> Result<()> {
        let a: T = self.fetch_immediate()?;
        let b: T = self.read_ea_hold(instr, instr.get_op2())?;
        self.ea_commit();
        let result = calcfn(a, b);

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs
            .sr
            .set_n(result.reverse_bits() & T::one() != T::zero());
        self.regs.sr.set_z(result == T::zero());

        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(4)?,
            _ => (),
        };

        Ok(())
    }

    /// AND/OR/EOR to CCR
    fn op_bitwise_ccr(
        &mut self,
        _instr: &Instruction,
        calcfn: fn(Byte, Byte) -> Byte,
    ) -> Result<()> {
        let a = self.fetch_immediate()?;
        let b = self.regs.sr.ccr();
        self.regs.sr.set_ccr(calcfn(a, b));

        // Idle cycles and dummy read
        self.advance_cycles(8)?;
        self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        self.prefetch_pump()?;

        Ok(())
    }

    /// AND/OR/EOR to SR
    fn op_bitwise_sr(
        &mut self,
        _instr: &Instruction,
        calcfn: fn(Word, Word) -> Word,
    ) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        let a = self.fetch_immediate()?;
        let b = self.regs.sr.sr();
        self.regs.sr.set_sr(calcfn(a, b));

        // Idle cycles and dummy read
        self.advance_cycles(8)?;
        self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        self.prefetch_pump()?;

        Ok(())
    }

    /// TRAP
    fn op_trap(&mut self, instr: &Instruction) -> Result<()> {
        self.raise_exception(
            ExceptionGroup::Group2,
            instr.trap_get_vector() * 4 + VECTOR_TRAP_OFFSET,
            None,
        )
    }

    /// ADD/SUB
    fn op_alu<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let left: T = self.regs.read_d(instr.get_op1());
        let right: T = self.read_ea_hold(instr, instr.get_op2())?;
        self.ea_commit();
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.regs.sr.set_ccr(ccr);
        self.prefetch_pump()?;
        match instr.get_direction() {
            Direction::Right => self.regs.write_d(instr.get_op1(), result),
            Direction::Left => self.write_ea(instr, instr.get_op2(), result)?,
        }

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => {
                self.advance_cycles(4)?
            }
            (AddressingMode::AddressRegister, _, 4) => self.advance_cycles(4)?,

            (_, Direction::Right, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// ADDI/SUBI
    fn op_alu_immediate<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let b: T = self.fetch_immediate()?;
        let a: T = self.read_ea_hold(instr, instr.get_op2())?;
        self.ea_commit();
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.regs.sr.set_ccr(ccr);
        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(4)?,
            (AddressingMode::AddressRegister, _, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// NEG/NEGX
    fn op_alu_zero<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let b: T = self.read_ea(instr, instr.get_op2())?;
        let a = T::zero();
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.regs.sr.set_ccr(ccr);
        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// ALU 'quick' group of instructions
    fn op_alu_quick<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let b: T = instr.get_quick();
        let a: T = self.read_ea_hold(instr, instr.get_op2())?;
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        if instr.get_addr_mode()? == AddressingMode::AddressRegister
            && std::mem::size_of::<T>() >= 2
        {
            // Word and longword operations on address registers do not affect condition codes.
        } else {
            self.regs.sr.set_ccr(ccr)
        }

        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;
        self.ea_commit();

        // Idle cycles
        match (instr.get_addr_mode()?, std::mem::size_of::<T>()) {
            (AddressingMode::DataRegister, 4) => self.advance_cycles(4)?,
            (AddressingMode::AddressRegister, 4) => self.advance_cycles(4)?,
            _ => (),
        };

        Ok(())
    }

    /// ALU address register group of instructions
    fn op_alu_a<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(Long, Long, RegisterSR) -> (Long, u8),
    ) -> Result<()> {
        let b = self
            .read_ea_hold::<T>(instr, instr.get_op2())?
            .expand_sign_extend();
        self.ea_commit();
        let a: Long = self.regs.read_a(instr.get_op1());
        let (result, _) = calcfn(a, b, self.regs.sr);

        // Flags are not changed
        self.prefetch_pump()?;
        self.regs.write_a::<Long>(instr.get_op1(), result);

        // Idle cycles
        match (instr.get_addr_mode()?, std::mem::size_of::<T>()) {
            (AddressingMode::AddressRegister, _) => self.advance_cycles(4)?,
            (AddressingMode::DataRegister, _) => self.advance_cycles(4)?,
            (AddressingMode::Immediate, _) => self.advance_cycles(4)?,
            (_, 2) => self.advance_cycles(4)?,
            (_, 4) => self.advance_cycles(2)?,
            _ => unreachable!(),
        };

        Ok(())
    }

    /// ALU 'X' group of instructions
    fn op_alu_x<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        // Skip normal addressing mode processing here, too different.
        let sz = std::mem::size_of::<T>();
        let (b, a): (T, T) = match (instr.get_addr_mode_x()?, sz) {
            (AddressingMode::DataRegister, _) => (
                self.regs.read_d(instr.get_op2()),
                self.regs.read_d(instr.get_op1()),
            ),
            (AddressingMode::IndirectPreDec, 4) => {
                self.advance_cycles(2)?;
                // The order here is very explicit due to the way registers need to be left if
                // an address error occurs.
                let a_addr_low = self.regs.read_a_predec(instr.get_op2(), 2);
                let a_low = self.read_ticks::<Word>(a_addr_low)? as Long;
                let a_addr_high = self.regs.read_a_predec(instr.get_op2(), 2);
                let a_high = self.read_ticks::<Word>(a_addr_high)? as Long;
                let b_addr_low = self.regs.read_a_predec(instr.get_op1(), 2);
                let b_low = self.read_ticks::<Word>(b_addr_low)? as Long;
                let b_addr_high = self.regs.read_a_predec(instr.get_op1(), 2);
                let b_high = self.read_ticks::<Word>(b_addr_high)? as Long;
                (
                    T::chop(a_low | (a_high << 16)),
                    T::chop(b_low | (b_high << 16)),
                )
            }
            (AddressingMode::IndirectPreDec, _) => {
                self.advance_cycles(2)?;
                // The order here is very explicit due to the way registers need to be left if
                // an address error occurs.
                let a_addr = self.regs.read_a_predec(instr.get_op2(), sz);
                let a = self.read_ticks(a_addr)?;
                let b_addr = self.regs.read_a_predec(instr.get_op1(), sz);
                let b = self.read_ticks(b_addr)?;
                (a, b)
            }
            _ => unreachable!(),
        };

        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.regs.sr.set_ccr(ccr);

        match (instr.get_addr_mode_x()?, sz) {
            (AddressingMode::DataRegister, _) => {
                self.prefetch_pump()?;
                self.regs.write_d(instr.get_op1(), result)
            }
            (AddressingMode::IndirectPreDec, 4) => {
                // This writes in 16-bit steps, with a prefetch in between..
                let result = result.expand();
                let addr_low = self.regs.read_a::<Address>(instr.get_op1()).wrapping_add(2);
                self.write_ticks::<Word>(addr_low, result as Word)?;

                self.prefetch_pump()?;

                let addr_high = self.regs.read_a(instr.get_op1());
                self.write_ticks::<Word>(addr_high, (result >> 16) as Word)?;
            }
            (AddressingMode::IndirectPreDec, _) => {
                self.prefetch_pump()?;
                let b_addr = self.regs.read_a(instr.get_op1());
                self.write_ticks(b_addr, result)?
            }
            _ => unreachable!(),
        };

        // Idle cycles
        match (instr.get_addr_mode_x()?, sz) {
            (AddressingMode::DataRegister, 4) => self.advance_cycles(4)?,
            _ => (),
        };

        Ok(())
    }

    /// CMP
    fn op_cmp<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let a: T = self.regs.read_d(instr.get_op1());
        let b: T = self.read_ea(instr, instr.get_op2())?;
        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        let last_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        // X is unchanged
        self.regs.sr.set_x(last_x);

        self.prefetch_pump()?;

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => {
                self.advance_cycles(2)?
            }
            (AddressingMode::AddressRegister, _, 4) => self.advance_cycles(2)?,

            (_, Direction::Right, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// CMPI
    fn op_cmp_immediate<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let b: T = self.fetch_immediate()?;
        let a: T = self.read_ea(instr, instr.get_op2())?;
        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        let last_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        // X is unchanged
        self.regs.sr.set_x(last_x);

        self.prefetch_pump()?;

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// CMPM
    fn op_cmpm<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let len = std::mem::size_of::<T>();
        let b_addr = self.regs.read_a(instr.get_op2());
        self.regs.read_a_postinc::<Address>(instr.get_op2(), len);
        let b: T = self.read_ticks(b_addr)?;
        let a_addr = self.regs.read_a(instr.get_op1());
        let a: T = self.read_ticks(a_addr)?;
        self.regs.read_a_postinc::<Address>(instr.get_op1(), len);
        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        let last_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        // X is unchanged
        self.regs.sr.set_x(last_x);

        self.prefetch_pump()?;

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(4)?,
            _ => (),
        };

        Ok(())
    }

    /// CMPA
    fn op_cmp_address<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let b = self
            .read_ea::<T>(instr, instr.get_op2())?
            .expand_sign_extend();
        let a: Long = self.regs.read_a(instr.get_op1());

        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        let old_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        self.regs.sr.set_x(old_x);

        self.prefetch_pump()?;
        self.advance_cycles(2)?; // 2x idle

        Ok(())
    }

    /// MULU
    fn op_mulu(&mut self, instr: &Instruction) -> Result<()> {
        let a = self.regs.read_d::<Word>(instr.get_op1()) as Long;
        let b = self.read_ea::<Word>(instr, instr.get_op2())? as Long;
        let result = a.wrapping_mul(b);

        self.prefetch_pump()?;

        // Computation time
        self.advance_cycles(34 + (b.count_ones() as Ticks) * 2)?;

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & 0x8000_0000 != 0);
        self.regs.sr.set_z(result == 0);

        self.regs.write_d(instr.get_op1(), result);

        Ok(())
    }

    /// MULS
    fn op_muls(&mut self, instr: &Instruction) -> Result<()> {
        let a = self.regs.read_d::<Word>(instr.get_op1()) as i16 as i32;
        let b = self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i32;
        let result = a.wrapping_mul(b) as Long;

        self.prefetch_pump()?;

        // Computation time
        self.advance_cycles(34 + (((b << 1) ^ b).count_ones() as Ticks) * 2)?;

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & 0x8000_0000 != 0);
        self.regs.sr.set_z(result == 0);

        self.regs.write_d(instr.get_op1(), result);

        Ok(())
    }

    /// DIVU
    fn op_divu(&mut self, instr: &Instruction) -> Result<()> {
        let mut dividend = self.regs.read_d::<Long>(instr.get_op1());
        let mut divisor = self.read_ea::<Word>(instr, instr.get_op2())? as Long;

        if divisor == 0 {
            // Division by zero
            self.advance_cycles(4)?;
            self.regs.sr.set_n(false);
            self.regs.sr.set_c(false);
            self.regs.sr.set_z(false);
            self.regs.sr.set_v(false);

            return self.raise_exception(ExceptionGroup::Group2, VECTOR_DIV_ZERO, None);
        }

        let result = dividend / divisor;
        let result_rem = dividend % divisor;

        self.regs.sr.set_c(false);
        self.regs.sr.set_z(false);
        self.advance_cycles(6)?;
        if result > Word::MAX.into() {
            // Overflow
            self.regs.sr.set_v(true);
            self.regs.sr.set_n(true);
            self.prefetch_pump()?;

            return Ok(());
        }

        // Simulate the cycle time
        self.advance_cycles(6)?;
        divisor <<= 16;
        let mut last_msb;
        for _ in 0..15 {
            self.advance_cycles(4)?;
            last_msb = dividend & 0x8000_0000 != 0;
            dividend <<= 1;
            if !last_msb {
                self.advance_cycles(2)?;
                if dividend < divisor {
                    self.advance_cycles(2)?;
                }
            }

            if last_msb || dividend >= divisor {
                dividend = dividend.wrapping_sub(divisor);
            }
        }

        self.prefetch_pump()?;

        self.regs.sr.set_z(result == 0);
        self.regs.sr.set_n(result & 0x8000 != 0);
        self.regs.sr.set_v(false);
        self.regs
            .write_d(instr.get_op1(), (result_rem << 16) | result);

        Ok(())
    }

    /// DIVS
    fn op_divs(&mut self, instr: &Instruction) -> Result<()> {
        let dividend = self.regs.read_d::<Long>(instr.get_op1()) as i32;
        let divisor = self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i32;

        if divisor == 0 {
            // Division by zero
            self.advance_cycles(4)?;
            self.regs.sr.set_n(false);
            self.regs.sr.set_c(false);
            self.regs.sr.set_z(false);
            self.regs.sr.set_v(false);

            return self.raise_exception(ExceptionGroup::Group2, VECTOR_DIV_ZERO, None);
        }

        let result = dividend / divisor;
        let result_rem = dividend % divisor;

        self.regs.sr.set_c(false);
        self.regs.sr.set_z(false);
        self.advance_cycles(8)?;
        if dividend < 0 {
            self.advance_cycles(2)?;
        }
        if dividend.wrapping_abs() >= (divisor.wrapping_abs() << 16) && divisor != i16::MIN as i32 {
            // Overflow (detected before calculation)
            self.advance_cycles(4)?;

            self.regs.sr.set_v(true);
            self.regs.sr.set_n(true);
            self.prefetch_pump()?;

            return Ok(());
        }

        // Simulate the cycle time
        if divisor < 0 {
            // +2 for negative divisor
            self.advance_cycles(2)?;
        } else if dividend < 0 {
            // +4 for positive divisor, negative dividend
            self.advance_cycles(4)?;
        }

        // Count zeroes in top 15 most significant bits
        let zeroes = ((result.wrapping_abs() as u16) | 1).count_zeros() as Ticks;
        self.advance_cycles(108 + zeroes * 2)?;

        if result > i16::MAX.into() || result < i16::MIN.into() {
            // Overflow (detected during calculation)
            self.regs.sr.set_v(true);
            self.regs.sr.set_n(true);
            self.prefetch_pump()?;

            return Ok(());
        }

        self.prefetch_pump()?;

        self.regs.sr.set_z(result == 0);
        self.regs.sr.set_n(result & 0x8000 != 0);
        self.regs.sr.set_v(false);
        self.regs.write_d(
            instr.get_op1(),
            (((result_rem as Long) << 16) & 0xFFFF_0000) | ((result as Long) & 0xFFFF),
        );

        Ok(())
    }

    /// BTST/BSET/BCHG/BCLR
    fn op_bit<const IMM: bool>(
        &mut self,
        instr: &Instruction,
        calcfn: Option<fn(Long, Long) -> Long>,
    ) -> Result<()> {
        let bitnum = if IMM {
            self.fetch_immediate::<Byte>()?
        } else {
            self.regs.read_d(instr.get_op1())
        };

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                self.prefetch_pump()?;
                let val: Long = self.read_ea(instr, instr.get_op2())?;
                let bit = 1_u32 << (bitnum % 32);
                self.regs.sr.set_z(val & bit == 0);
                self.advance_cycles(2)?;
                if let Some(cf) = calcfn {
                    self.write_ea(instr, instr.get_op2(), cf(val, bit))?;
                    if bitnum % 32 > 15 {
                        self.advance_cycles(2)?;
                    }
                    if instr.mnemonic == InstructionMnemonic::BCLR_dn
                        || instr.mnemonic == InstructionMnemonic::BCLR_imm
                    {
                        // :'(
                        self.advance_cycles(2)?;
                    }
                }
            }
            _ => {
                let val: Byte = self.read_ea(instr, instr.get_op2())?;
                let bit = 1_u8 << (bitnum % 8);
                self.regs.sr.set_z(val & bit == 0);
                self.prefetch_pump()?;
                if let Some(cf) = calcfn {
                    self.write_ea(instr, instr.get_op2(), cf(val as Long, bit.into()) as Byte)?;
                }
            }
        };

        // Idle cycles
        if instr.get_addr_mode()? == AddressingMode::Immediate {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// MOVEP
    fn op_movep<const N: usize, T>(&mut self, instr: &Instruction) -> Result<()>
    where
        T: FromBytes<Bytes = [u8; N]> + CpuSized,
    {
        instr.fetch_extword(|| self.fetch_pump())?;
        let addr: Address = self
            .regs
            .read_a::<Address>(instr.get_op2())
            .wrapping_add_signed(instr.get_displacement()?)
            & ADDRESS_MASK;

        if instr.get_direction_movep() == Direction::Right {
            // To bus
            let data = self.regs.read_d::<T>(instr.get_op1()).to_be_bytes();

            for (i, b) in data.as_ref().iter().cloned().enumerate() {
                let b_addr = addr.wrapping_add((i * 2) as Address);
                self.write_ticks::<Byte>(b_addr, b)?;
            }
        } else {
            // From bus
            let mut data = [0; N];
            for i in 0..N {
                let b_addr = addr.wrapping_add((i * 2) as Address);
                data[i] = self.read_ticks::<Byte>(b_addr)?;
            }

            self.regs
                .write_d::<T>(instr.get_op1(), T::from_be_bytes(&data));
        }

        Ok(())
    }

    /// MOVEA
    fn op_movea<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let value: T = self.read_ea(instr, instr.get_op2())?;
        self.regs.write_a(instr.get_op1(), value);
        Ok(())
    }

    /// MOVE
    fn op_move<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let value: T = self.read_ea_with(instr, instr.get_addr_mode()?, instr.get_op2(), false)?;

        self.regs.sr.set_z(value == T::zero());
        self.regs.sr.set_n(value & T::msb() != T::zero());
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);

        // Clear EA cache
        // TODO this is kinda hacky
        self.step_ea_addr = None;
        instr.clear_extword();

        match (instr.get_addr_mode_left()?, instr.get_addr_mode()?) {
            (AddressingMode::IndirectPreDec, _) => {
                // MOVE ..., -(An) this mode has a fetch instead of the idle cycles.
                let addr: Address = self
                    .regs
                    .read_a_predec(instr.get_op1(), std::mem::size_of::<T>());
                self.prefetch_pump()?;
                self.write_ticks(addr, value)?
            }
            (
                AddressingMode::AbsoluteLong,
                AddressingMode::Indirect
                | AddressingMode::IndirectDisplacement
                | AddressingMode::IndirectIndex
                | AddressingMode::IndirectPostInc
                | AddressingMode::IndirectPreDec
                | AddressingMode::PCIndex
                | AddressingMode::PCDisplacement
                | AddressingMode::AbsoluteShort
                | AddressingMode::AbsoluteLong,
            ) => {
                // This is for MOVE ..., (xxx).l, which interleaves the prefetches with the write.
                // Do the write in between the prefetches, so preload the EA cache manually.
                let h = self.fetch()? as u32;
                let l = self.fetch()? as u32;
                self.step_ea_addr = Some((h << 16) | l);
                self.write_ea_with(
                    instr,
                    instr.get_addr_mode_left()?,
                    instr.get_op1(),
                    value,
                    TemporalOrder::LowToHigh,
                    false,
                )?
            }
            _ => self.write_ea_with(
                instr,
                instr.get_addr_mode_left()?,
                instr.get_op1(),
                value,
                TemporalOrder::LowToHigh,
                false,
            )?,
        }

        Ok(())
    }

    /// MOVEfromSR
    fn op_move_from_sr(&mut self, instr: &Instruction) -> Result<()> {
        let value = self.regs.sr.sr();

        // Discarded read, prefetch
        self.read_ea::<Word>(instr, instr.get_op2())?;
        self.prefetch_pump()?;

        self.write_ea(instr, instr.get_op2(), value)?;

        // Idle cycles
        match instr.get_addr_mode()? {
            AddressingMode::DataRegister | AddressingMode::AddressRegister => {
                self.advance_cycles(2)?
            }
            _ => (),
        }

        Ok(())
    }

    /// MOVEtoSR
    fn op_move_to_sr(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_exception(ExceptionGroup::Group1, VECTOR_PRIVILEGE_VIOLATION, None);
        }
        let value: Word = self.read_ea(instr, instr.get_op2())?;

        // Idle cycles and discarded read
        self.advance_cycles(4)?;
        self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;

        self.regs.sr.set_sr(value);
        Ok(())
    }

    /// MOVEtoCCR
    fn op_move_to_ccr(&mut self, instr: &Instruction) -> Result<()> {
        let value: Word = self.read_ea(instr, instr.get_op2())?;

        // Idle cycles + discarded read
        self.advance_cycles(4)?;
        self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        self.prefetch_pump()?;

        self.regs.sr.set_ccr(value as Byte);
        Ok(())
    }

    /// MOVEtoUSP
    fn op_move_to_usp(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_exception(ExceptionGroup::Group1, VECTOR_PRIVILEGE_VIOLATION, None);
        }
        let value: Address = self.regs.read_a(instr.get_op2());

        self.regs.usp = value;
        Ok(())
    }

    /// MOVEfromUSP
    fn op_move_from_usp(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_exception(ExceptionGroup::Group1, VECTOR_PRIVILEGE_VIOLATION, None);
        }
        let value: Address = self.regs.usp;

        // Idle cycles and discarded read
        self.regs.write_a(instr.get_op2(), value);
        Ok(())
    }

    /// CLR
    fn op_clr<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        self.read_ea::<T>(instr, instr.get_op2())?;

        self.prefetch_pump()?;

        self.regs.sr.set_n(false);
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_z(true);
        self.write_ea(instr, instr.get_op2(), T::zero())?;

        // Idle cycles
        if std::mem::size_of::<T>() == 4 {
            match instr.get_addr_mode()? {
                AddressingMode::DataRegister | AddressingMode::AddressRegister => {
                    self.advance_cycles(2)?
                }
                _ => (),
            }
        }

        Ok(())
    }

    /// NOT
    fn op_not<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let result: T = !self.read_ea(instr, instr.get_op2())?;

        self.prefetch_pump()?;

        self.regs.sr.set_n(result & T::msb() != T::zero());
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_z(result == T::zero());
        self.write_ea(instr, instr.get_op2(), result)?;

        // Idle cycles
        if std::mem::size_of::<T>() == 4 {
            match instr.get_addr_mode()? {
                AddressingMode::DataRegister | AddressingMode::AddressRegister => {
                    self.advance_cycles(2)?
                }
                _ => (),
            }
        }
        Ok(())
    }

    /// EXT
    fn op_ext<T: CpuSized, U: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        // T: dest type, U: src type
        let value: U = self.read_ea(instr, instr.get_op2())?;
        let result = T::chop(value.expand_sign_extend());

        self.regs.sr.set_n(result & T::msb() != T::zero());
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_z(result == T::zero());
        self.write_ea::<T>(instr, instr.get_op2(), result)?;
        Ok(())
    }

    /// SBCD
    fn op_sbcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_x::<Byte>(&instr, Self::alu_sub_bcd)?;
        if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// ABCD
    fn op_abcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_x::<Byte>(&instr, Self::alu_add_bcd)?;
        if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// NBCD
    fn op_nbcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_zero::<Byte>(&instr, Self::alu_sub_bcd)?;
        if instr.get_addr_mode()? == AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// PEA
    fn op_pea(&mut self, instr: &Instruction) -> Result<()> {
        let value: Long =
            self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;

        match instr.get_addr_mode()? {
            AddressingMode::IndirectIndex | AddressingMode::PCIndex => {
                self.advance_cycles(2)?;
                self.prefetch_pump()?;
            }
            AddressingMode::AbsoluteShort | AddressingMode::AbsoluteLong => (),
            _ => self.prefetch_pump()?,
        }

        let addr = self.regs.read_a_predec(7, std::mem::size_of::<Long>());
        self.write_ticks(addr, value)?;

        Ok(())
    }

    /// TAS
    pub fn op_tas(&mut self, instr: &Instruction) -> Result<()> {
        let v = self.read_ea::<Byte>(instr, instr.get_op2())?;
        if instr.get_addr_mode()? != AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }
        self.write_ea(instr, instr.get_op2(), v | 0x80)?;
        self.regs.sr.set_z(v == 0);
        self.regs.sr.set_n(v & 0x80 != 0);
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);
        Ok(())
    }

    /// TST
    fn op_tst<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let result: T = self.read_ea(instr, instr.get_op2())?;
        self.regs.sr.set_z(result == T::zero());
        self.regs.sr.set_n(result & T::msb() != T::zero());
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);
        Ok(())
    }

    /// LINK
    pub fn op_link(&mut self, instr: &Instruction) -> Result<()> {
        let sp = self.regs.read_a::<Address>(7).wrapping_sub(4);
        let addr = self.regs.read_a::<Address>(instr.get_op2());

        instr.fetch_extword(|| self.fetch_pump())?;

        self.write_ticks(sp, addr)?;
        self.regs.write_a(instr.get_op2(), sp);
        self.regs.read_a_predec::<Address>(7, 4);
        self.regs
            .write_a(7, sp.wrapping_add_signed(instr.get_displacement()?));

        Ok(())
    }

    /// UNLINK
    pub fn op_unlink(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.regs.read_a::<Address>(instr.get_op2());
        let val = self.read_ticks::<Address>(addr)?;
        self.regs.write_a(7, addr.wrapping_add(4));
        self.regs.write_a(instr.get_op2(), val);
        Ok(())
    }
}

impl<TBus> Tickable for CpuM68k<TBus>
where
    TBus: Bus<Address, u8>,
{
    fn tick(&mut self, _ticks: Ticks) -> Result<Ticks> {
        self.step()?;

        Ok(0)
    }
}
