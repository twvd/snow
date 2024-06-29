use std::collections::VecDeque;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::{Address, Bus, ADDRESS_MASK};
use crate::tickable::{Tickable, Ticks};

use super::instruction::{
    AddressingMode, Direction, IndexSize, Instruction, InstructionMnemonic, Xn,
};
use super::regs::{RegisterFile, RegisterSR};
use super::{Byte, CpuSized, Long, Word};

/// Access error details
#[derive(Debug, Clone, Copy)]
struct AccessError {
    function_code: u8,
    read: bool,
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
/// Privilege violation exception vector
const VECTOR_PRIVILEGE_VIOLATION: Address = 0x000020;
/// Trap exception vector offset (15 vectors)
const VECTOR_TRAP_OFFSET: Address = 0x000080;

/// Motorola 680x0
#[derive(Serialize, Deserialize)]
pub struct CpuM68k<TBus: Bus<Address>> {
    pub bus: TBus,
    pub regs: RegisterFile,
    pub cycles: Ticks,
    pub prefetch: VecDeque<u16>,

    step_ea_addr: Option<Address>,
}

impl<TBus> CpuM68k<TBus>
where
    TBus: Bus<Address>,
{
    pub fn new(bus: TBus) -> Self {
        Self {
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            prefetch: VecDeque::with_capacity(3),
            step_ea_addr: None,
        }
    }

    fn prefetch_pump(&mut self) -> Result<()> {
        if self.prefetch.len() >= 2 {
            return Ok(());
        }

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
        let v = self.prefetch.pop_front().unwrap();
        self.prefetch_pump()?;
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

        let instr = Instruction::try_decode(|| self.fetch())?;
        // TODO decoded instruction cache

        match self.execute_instruction(&instr) {
            Ok(()) => (),
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

    /// Reads a value from the bus and spends ticks.
    fn read_ticks<T: CpuSized>(&mut self, addr: Address) -> Result<T> {
        let len = std::mem::size_of::<T>();
        let mut result: T = T::zero();

        if len >= 2 && (addr & 1) != 0 {
            // Unaligned access
            bail!(CpuError::AccessError(AccessError {
                function_code: 0,
                ir: 0,

                // TODO instruction bit
                instruction: false,
                read: true,
                address: addr
            }));
        }

        // Below converts from BE -> LE on the fly
        for a in 0..len {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b: T = self.bus.read(byte_addr).into();
            result = result.wrapping_shl(8) | b;

            self.advance_cycles(2)?;
        }

        if len == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(result)
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    fn write_ticks<T: CpuSized>(&mut self, addr: Address, value: T) -> Result<()> {
        let mut val: Long = value.to_be().into();

        for a in 0..std::mem::size_of::<T>() {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b = val as u8;
            val = val >> 8;

            self.bus.write(byte_addr, b);
            self.advance_cycles(2)?;
        }

        if std::mem::size_of::<T>() == 1 {
            // Minimum of 4 cycles
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// Writes a value to the bus (big endian) and spends ticks.
    /// High-to-low temporal order.
    fn write_ticks_th<T: CpuSized>(&mut self, addr: Address, value: T) -> Result<()> {
        let mut val: Long = value.into();

        for a in (0..std::mem::size_of::<T>()).rev() {
            let byte_addr = addr.wrapping_add(a as Address) & ADDRESS_MASK;
            let b = val as u8;
            val = val >> 8;

            self.bus.write(byte_addr, b);
            self.advance_cycles(2)?;
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
        self.advance_cycles(4)?; // idle

        // Resume in supervisor mode
        self.regs.sr.set_supervisor(true);

        // Write exception stack frame
        match group {
            ExceptionGroup::Group0 => {
                let details = details.expect("Address error details not passed");

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
                    let res = self.op_alu_quick::<Long>(&instr, Self::alu_sub);
                    if let Ok(_) = res {
                        // ..and adds extra cycles?
                        self.advance_cycles(2)?;
                    }
                    res
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
                    let res = self.op_alu_quick::<Long>(&instr, Self::alu_add);
                    if let Ok(_) = res {
                        // ..and adds extra cycles?
                        self.advance_cycles(2)?;
                    }
                    res
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
            InstructionMnemonic::CMP_l => self.op_cmp::<Long>(&instr),
            InstructionMnemonic::CMP_w => self.op_cmp::<Word>(&instr),
            InstructionMnemonic::CMP_b => self.op_cmp::<Byte>(&instr),
            InstructionMnemonic::CMPI_l => self.op_cmp_immediate::<Long>(&instr),
            InstructionMnemonic::CMPI_w => self.op_cmp_immediate::<Word>(&instr),
            InstructionMnemonic::CMPI_b => self.op_cmp_immediate::<Byte>(&instr),
            InstructionMnemonic::CMPM_l => self.op_cmpm::<Long>(&instr),
            InstructionMnemonic::CMPM_w => self.op_cmpm::<Word>(&instr),
            InstructionMnemonic::CMPM_b => self.op_cmpm::<Byte>(&instr),
            InstructionMnemonic::NOP => Ok(()),
            InstructionMnemonic::SWAP => self.op_swap(&instr),
            InstructionMnemonic::TRAP => self.op_trap(&instr),
        }
    }

    /// Calculates address from effective addressing mode
    /// Happens once per instruction so e.g. postinc/predec only occur once.
    fn calc_ea_addr<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize) -> Result<Address> {
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
        let addr = match instr.get_addr_mode()? {
            AddressingMode::DataRegister => unreachable!(),
            AddressingMode::AddressRegister => unreachable!(),
            AddressingMode::Indirect => self.regs.read_a(ea_in),
            AddressingMode::IndirectPreDec => {
                self.advance_cycles(2)?; // 2x idle
                self.regs.read_a_predec(ea_in, std::mem::size_of::<T>())
            }
            AddressingMode::IndirectPostInc => self
                .regs
                .read_a_postinc::<Address>(ea_in, std::mem::size_of::<T>()),
            AddressingMode::IndirectDisplacement => {
                instr.fetch_extwords(|| self.fetch_pump())?;
                let addr = self.regs.read_a::<Address>(ea_in);
                let displacement = instr.get_displacement()?;
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::IndirectIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extwords(|| self.fetch_pump())?;

                let extword = instr.get_extword(0)?;
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
                instr.fetch_extwords(|| self.fetch_pump())?;
                let addr = self.regs.pc;
                let displacement = instr.get_displacement()?;
                Address::from(addr.wrapping_add_signed(displacement))
            }
            AddressingMode::PCIndex => {
                self.advance_cycles(2)?; // 2x idle
                instr.fetch_extwords(|| self.fetch_pump())?;
                let extword = instr.get_extword(0)?;
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

    /// Reads a value from the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn read_ea<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize) -> Result<T> {
        let v = match instr.get_addr_mode()? {
            AddressingMode::DataRegister => self.regs.read_d(ea_in),
            AddressingMode::AddressRegister => self.regs.read_a(ea_in),
            AddressingMode::Immediate => self.fetch_immediate::<T>()?,
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectPreDec
            | AddressingMode::IndirectPostInc
            | AddressingMode::PCDisplacement
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr::<T>(instr, ea_in)?;
                self.read_ticks(addr)?
            }
            AddressingMode::IndirectIndex | AddressingMode::PCIndex => {
                let addr = self.calc_ea_addr::<T>(instr, ea_in)?;
                self.read_ticks(addr)?
            }
        };

        Ok(v)
    }

    /// Writes a value to the operand (ea_in) using the effective addressing mode specified
    /// by the instruction, directly or through indirection, depending on the mode.
    fn write_ea<T: CpuSized>(&mut self, instr: &Instruction, ea_in: usize, value: T) -> Result<()> {
        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => Ok(self.regs.write_d(ea_in, value)),
            AddressingMode::AddressRegister => Ok(self.regs.write_a(ea_in, value)),
            AddressingMode::Indirect
            | AddressingMode::IndirectDisplacement
            | AddressingMode::IndirectIndex
            | AddressingMode::IndirectPreDec
            | AddressingMode::IndirectPostInc
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => {
                let addr = self.calc_ea_addr::<T>(instr, ea_in)?;
                self.write_ticks_th(addr, value)
            }
            _ => todo!(),
        }
    }

    /// SWAP
    pub fn op_swap(&mut self, instr: &Instruction) -> Result<()> {
        let v: Long = self.regs.read_d(instr.get_op2());
        let result = (v >> 16) | (v << 16);

        self.regs.write_d(instr.get_op2(), result);
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & (1 << 31) != 0);
        self.regs.sr.set_z(result == 0);

        Ok(())
    }

    /// AND/OR/EOR
    pub fn op_bitwise<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T) -> T,
    ) -> Result<()> {
        let left: T = self.regs.read_d(instr.get_op1());
        let right: T = self.read_ea(instr, instr.get_op2())?;
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let result = calcfn(a, b);

        self.prefetch_pump()?;
        match instr.get_direction() {
            Direction::Right => self.regs.write_d(instr.get_op1(), result),
            Direction::Left => self.write_ea(instr, instr.get_op2(), result)?,
        }
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs
            .sr
            .set_n(result.reverse_bits() & T::one() != T::zero());
        self.regs.sr.set_z(result == T::zero());

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
    pub fn op_bitwise_immediate<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T) -> T,
    ) -> Result<()> {
        let a: T = self.fetch_immediate()?;
        let b: T = self.read_ea(instr, instr.get_op2())?;
        let result = calcfn(a, b);

        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs
            .sr
            .set_n(result.reverse_bits() & T::one() != T::zero());
        self.regs.sr.set_z(result == T::zero());

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
    pub fn op_bitwise_ccr(
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
    pub fn op_bitwise_sr(
        &mut self,
        _instr: &Instruction,
        calcfn: fn(Word, Word) -> Word,
    ) -> Result<()> {
        if !self.regs.sr.supervisor() {
            // + TODO write test for privilege exception
            panic!("TODO privilege exception");
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
    pub fn op_trap(&mut self, instr: &Instruction) -> Result<()> {
        self.raise_exception(
            ExceptionGroup::Group2,
            instr.trap_get_vector() * 4 + VECTOR_TRAP_OFFSET,
            None,
        )?;
        Ok(())
    }

    /// ADD/SUB
    pub fn op_alu<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let left: T = self.regs.read_d(instr.get_op1());
        let right: T = self.read_ea(instr, instr.get_op2())?;
        let (a, b) = match instr.get_direction() {
            Direction::Right => (left, right),
            Direction::Left => (right, left),
        };
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.prefetch_pump()?;
        match instr.get_direction() {
            Direction::Right => self.regs.write_d(instr.get_op1(), result),
            Direction::Left => self.write_ea(instr, instr.get_op2(), result)?,
        }
        self.regs.sr.set_ccr(ccr);

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
    pub fn op_alu_immediate<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let b: T = self.fetch_immediate()?;
        let a: T = self.read_ea(instr, instr.get_op2())?;
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;
        self.regs.sr.set_ccr(ccr);

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

    /// ALU 'quick' group of instructions
    pub fn op_alu_quick<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, T, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let b: T = instr.get_quick();
        let a: T = self.read_ea(instr, instr.get_op2())?;
        let (result, ccr) = calcfn(a, b, self.regs.sr);

        self.prefetch_pump()?;
        self.write_ea(instr, instr.get_op2(), result)?;

        if instr.get_addr_mode()? == AddressingMode::AddressRegister
            && std::mem::size_of::<T>() >= 2
        {
            // Word and longword operations on address registers do not affect condition codes.
        } else {
            self.regs.sr.set_ccr(ccr)
        }

        // Idle cycles
        match (
            instr.get_addr_mode()?,
            instr.get_direction(),
            std::mem::size_of::<T>(),
        ) {
            (AddressingMode::DataRegister, _, 4) => self.advance_cycles(4)?,
            (AddressingMode::AddressRegister, _, _) => self.advance_cycles(2)?,
            _ => (),
        };

        Ok(())
    }

    /// ALU address register group of instructions
    pub fn op_alu_a<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(Long, Long, RegisterSR) -> (Long, u8),
    ) -> Result<()> {
        let b = self
            .read_ea::<T>(instr, instr.get_op2())?
            .expand_sign_extend();
        let a: Long = self.regs.read_a(instr.get_op1());
        let (result, _) = calcfn(a, b, self.regs.sr);

        self.prefetch_pump()?;
        self.regs.write_a::<Long>(instr.get_op1(), result);

        // Flags are not changed

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

    /// CMP
    pub fn op_cmp<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let a: T = self.regs.read_d(instr.get_op1());
        let b: T = self.read_ea(instr, instr.get_op2())?;
        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        self.prefetch_pump()?;
        let last_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        // X is unchanged
        self.regs.sr.set_x(last_x);

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
    pub fn op_cmp_immediate<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let b: T = self.fetch_immediate()?;
        let a: T = self.read_ea(instr, instr.get_op2())?;
        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        self.prefetch_pump()?;
        let last_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        // X is unchanged
        self.regs.sr.set_x(last_x);

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
    pub fn op_cmpm<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let len = std::mem::size_of::<T>();
        let b_addr = self.regs.read_a_postinc(instr.get_op2(), len);
        let b: T = self.read_ticks(b_addr)?;
        let a_addr = self.regs.read_a_postinc(instr.get_op1(), len);
        let a: T = self.read_ticks(a_addr)?;
        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        self.prefetch_pump()?;
        let last_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        // X is unchanged
        self.regs.sr.set_x(last_x);

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
}

impl<TBus> Tickable for CpuM68k<TBus>
where
    TBus: Bus<Address>,
{
    fn tick(&mut self, _ticks: Ticks) -> Result<Ticks> {
        self.step()?;

        Ok(0)
    }
}
