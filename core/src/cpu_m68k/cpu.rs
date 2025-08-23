use std::collections::VecDeque;

use anyhow::{bail, Result};
use arrayvec::ArrayVec;
use either::Either;
use log::*;
use num_traits::{FromBytes, PrimInt, ToBytes};
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::fpu::regs::FpuRegisterFile;
use crate::cpu_m68k::pmmu::regs::PmmuRegisterFile;
use crate::cpu_m68k::regs::RegisterCACR;
use crate::cpu_m68k::M68000_SR_MASK;
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, Long, Word};

use super::instruction::{
    AddressingMode, BfxExtWord, Direction, DivlExtWord, Instruction, InstructionMnemonic,
    MulxExtWord,
};
use super::regs::{Register, RegisterFile, RegisterSR};
use super::{
    CpuM68kType, CpuSized, M68000, M68010, M68020, M68020_SR_MASK, TORDER_HIGHLOW, TORDER_LOWHIGH,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BusBreakpoint {
    Read,
    Write,
    ReadWrite,
}

/// A breakpoint
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum Breakpoint {
    /// Breaks when program counter reaches address
    Execution(Address),
    /// Breaks when a bus read/write occurs on address
    Bus(BusBreakpoint, Address),
    /// Breaks when an interrupt of specified level occurs
    InterruptLevel(u8),
    /// Breaks when CPU jumps to specified exception vector
    ExceptionVector(Address),
    /// Breaks on a LINEA instruction of specified opcode
    LineA(u16),
    /// Breaks on a LINEF instruction of specified opcode
    LineF(u16),
    /// Breakpoint for step over (self-clearing)
    StepOver(Address),
    /// Breakpoint for step out
    /// Address is stack pointer
    StepOut(Address),
}

/// Address error/bus error details
#[derive(Debug, Clone, Copy)]
pub(in crate::cpu_m68k) struct Group0Details {
    #[allow(dead_code)]
    pub(in crate::cpu_m68k) function_code: u8,
    pub(in crate::cpu_m68k) read: bool,
    #[allow(dead_code)]
    pub(in crate::cpu_m68k) instruction: bool,
    pub(in crate::cpu_m68k) address: Address,
    pub(in crate::cpu_m68k) ir: Word,
    pub(in crate::cpu_m68k) start_pc: Address,
    pub(in crate::cpu_m68k) size: usize,
}

/// CPU error type to cascade exceptions down
#[derive(Error, Debug)]
pub(in crate::cpu_m68k) enum CpuError {
    /// Raise address error exception (unaligned address on Word/Long access)
    #[error("Address error exception")]
    AddressError(Group0Details),
    /// Raise bus error exception
    #[error("Bus error exception")]
    BusError(Group0Details),
    /// Handle page fault (PMMU)
    #[error("Page fault")]
    Pagefault,
}

/// M68000 exception groups
#[derive(Debug, Clone, Copy)]
pub(in crate::cpu_m68k) enum ExceptionGroup {
    Group0,
    Group1,
    Group2,
}

bitfield! {
    /// 68020+ group 0 exception stack frame Special Status Word
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct Group0Ssw(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        pub function_code: u8 @ 0..=2,

        /// Size of failed access
        /// 0 = Long, 1 = Word, 2 = Byte
        pub size: usize @ 4..=5,

        pub read: bool @ 6,
        /// Read-Modify-Write
        pub rm: bool @ 7,
        /// Re-run fault
        pub df: bool @ 8,
        /// Re-run stage B
        pub rb: bool @ 12,
        /// Re-run stage C
        pub rc: bool @ 13,
        /// Fault on stage B
        pub fb: bool @ 14,
        /// Fault on stage C
        pub fc: bool @ 15,
    }
}

// Exception vectors
/// Stack pointer initialization
pub const VECTOR_SP: Address = 0x00000000;
/// Reset vector
pub const VECTOR_RESET: Address = 0x00000004;
/// Bus error exception vector
pub const VECTOR_BUS_ERROR: Address = 0x000008;
/// Address error exception vector
pub const VECTOR_ADDRESS_ERROR: Address = 0x00000C;
/// Illegal instruction exception vector
pub const VECTOR_ILLEGAL: Address = 0x000010;
/// Division by zero exception vector
pub const VECTOR_DIV_ZERO: Address = 0x000014;
/// CHK exception vector
pub const VECTOR_CHK: Address = 0x000018;
/// TRAPV exception vector
pub const VECTOR_TRAPV: Address = 0x00001C;
/// Privilege violation exception vector
pub const VECTOR_PRIVILEGE_VIOLATION: Address = 0x000020;
/// Trace exception
pub const VECTOR_TRACE: Address = 0x000024;
/// Line 1010 / A
pub const VECTOR_LINEA: Address = 0x000028;
/// Line 1111 / F
pub const VECTOR_LINEF: Address = 0x00002C;
/// Auto vector offset (7 vectors)
pub const VECTOR_AUTOVECTOR_OFFSET: Address = 0x000064;
/// Trap exception vector offset (15 vectors)
pub const VECTOR_TRAP_OFFSET: Address = 0x000080;

/// Register mask order for MOVEM
const MOVEM_REGS: [Register; 16] = [
    Register::An(7),
    Register::An(6),
    Register::An(5),
    Register::An(4),
    Register::An(3),
    Register::An(2),
    Register::An(1),
    Register::An(0),
    Register::Dn(7),
    Register::Dn(6),
    Register::Dn(5),
    Register::Dn(4),
    Register::Dn(3),
    Register::Dn(2),
    Register::Dn(1),
    Register::Dn(0),
];

/// Instruction decode cache. Each opcode (16-bit) has a slot, index = opcode.
type DecodeCache = Vec<Option<Instruction>>;

/// Creates an empty instruction cache
fn empty_decode_cache() -> DecodeCache {
    vec![None; Word::MAX as usize + 1]
}

/// I-cache cache line size (68020/68030)
pub const ICACHE_LINE_SIZE: usize = 4;

/// I-cache amount of cache lines (68020/68030)
pub const ICACHE_LINES: usize = 64;

/// I-cache tag marking a line as invalid
///
/// Tags are normally 30-bit, we keep 32.
/// We store the line address as full width address, with the lower
/// (index/offset) bits masked to 0. This is why this value works as an
/// 'invalid' marker, as the lower 8 bits are normally always 0.
pub const ICACHE_TAG_INVALID: u32 = 0xFFFF_FFFF;

pub const ICACHE_TAG_MASK: Address = 0xFFFF_FF00;
pub const ICACHE_INDEX_MASK: Address = 0x0000_00FC;
pub const ICACHE_OFFSET_MASK: Address = 0x000_0003;

#[derive(Clone, Eq, PartialEq)]
// The point of this enum is to mostly store instructions without allocation
#[allow(clippy::large_enum_variant)]
pub enum HistoryEntry {
    Instruction(HistoryEntryInstruction),
    Exception { vector: Address, cycles: Ticks },
    Pagefault { address: Address, write: bool },
}

#[derive(Default, Clone, PartialEq, Eq)]
pub struct HistoryEntryInstruction {
    pub pc: Address,
    pub raw: ArrayVec<u8, 24>,
    pub cycles: Ticks,
    pub initial_regs: Option<RegisterFile>,
    pub final_regs: Option<RegisterFile>,
    pub branch_taken: Option<bool>,
    pub waitstates: bool,
    pub ea: Option<Address>,
    pub icache_hit: bool,
    pub icache_miss: bool,
}

impl HistoryEntryInstruction {
    pub fn push_raw<T: ToBytes>(&mut self, value: &T) {
        self.raw
            .try_extend_from_slice(value.to_be_bytes().as_ref())
            .expect("HistoryEntry::raw overrun");
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct SystrapHistoryEntry {
    pub trap: Word,
    pub cycles: Ticks,
    pub pc: Address,
}

/// Motorola 680x0
pub struct CpuM68k<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Exception occured this step
    pub step_exception: bool,

    /// External address/data bus
    pub bus: TBus,

    /// Register state
    pub regs: RegisterFile,

    /// Total cycle counter
    pub cycles: Ticks,

    /// Current prefetch queue
    pub prefetch: VecDeque<u16>,

    /// Effective Address cache for this step
    pub(in crate::cpu_m68k) step_ea_addr: Option<Address>,

    /// Value to load to an address register by ea_commit().
    pub(in crate::cpu_m68k) step_ea_load: Option<(usize, Address)>,

    /// Instruction decode cache
    //#[serde(skip, default = "empty_decode_cache")]
    decode_cache: DecodeCache,

    /// Mask trace exceptions (for tests)
    pub trace_mask: bool,

    /// Active breakpoints
    pub(in crate::cpu_m68k) breakpoints: Vec<Breakpoint>,

    /// Breakpoint hit latch
    //#[serde(skip)]
    pub(in crate::cpu_m68k) breakpoint_hit: LatchingEvent,

    /// Next address to jump to for step over
    step_over_addr: Option<Address>,

    /// Instruction history
    //#[serde(skip)]
    pub(in crate::cpu_m68k) history: VecDeque<HistoryEntry>,

    /// Current history item
    //#[serde(skip)]
    pub(in crate::cpu_m68k) history_current: HistoryEntryInstruction,

    /// Keep history?
    //#[serde(skip)]
    pub(in crate::cpu_m68k) history_enabled: bool,

    /// System trap history
    //#[serde(skip)]
    systrap_history: VecDeque<SystrapHistoryEntry>,
    systrap_history_enabled: bool,

    /// PMMU address translation caches
    /// The index is either PMMU_ATC_URP or PMMU_ATC_SRP, depending on which root
    /// pointer is used in the translation.
    ///
    /// The caching method used is a one-dimensional lookup table where the
    /// index is the page, allowing for O(1) lookup. The cache is expanded on
    /// the fly (never shrunk).
    //#[serde(skip)]
    pub(in crate::cpu_m68k) pmmu_atc: [Vec<Option<Address>>; 2],

    /// 68020+ I-cache lines
    icache_lines: [[u8; ICACHE_LINE_SIZE]; ICACHE_LINES],

    /// 68020+ I-cache tags
    icache_tags: [u32; ICACHE_LINES],

    /// Register state at the beginning of an instruction to allow
    /// restarting an instruction that caused a mid-instruction
    /// bus fault.
    pub(in crate::cpu_m68k) restart_regs: Option<RegisterFile>,
}

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Instruction history size
    pub const HISTORY_SIZE: usize = 10000;

    pub fn new(bus: TBus) -> Self {
        assert!([M68000, M68020].contains(&CPU_TYPE));

        Self {
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            prefetch: VecDeque::with_capacity(3),
            step_ea_addr: None,
            step_exception: false,
            step_ea_load: None,
            decode_cache: empty_decode_cache(),
            trace_mask: false,
            breakpoints: vec![],
            breakpoint_hit: LatchingEvent::default(),
            step_over_addr: None,
            history: VecDeque::with_capacity(Self::HISTORY_SIZE),
            history_current: HistoryEntryInstruction::default(),
            history_enabled: false,
            systrap_history: VecDeque::with_capacity(Self::HISTORY_SIZE),
            systrap_history_enabled: false,
            pmmu_atc: Default::default(),
            icache_lines: core::array::from_fn(|_| Default::default()),
            icache_tags: [ICACHE_TAG_INVALID; ICACHE_LINES],
            restart_regs: None,
        }
    }

    pub const fn get_type(&self) -> CpuM68kType {
        CPU_TYPE
    }

    /// Resets the CPU, loads reset vector and initial SP
    pub fn reset(&mut self) -> Result<()> {
        self.regs = RegisterFile::new();
        self.icache_tags.fill(ICACHE_TAG_INVALID);

        self.cycles = 0;
        let init_ssp = self.read_ticks(VECTOR_SP)?;
        let init_pc = self.read_ticks(VECTOR_RESET)?;

        info!("Reset - SSP: {:08X}, PC: {:08X}", init_ssp, init_pc);
        self.regs.isp = init_ssp;
        self.regs.sr.set_supervisor(true);
        self.regs.sr.set_int_prio_mask(7);
        self.set_pc(init_pc)?;
        self.prefetch_refill()?;

        Ok(())
    }

    /// Tests if a breakpoint was hit
    pub fn get_clr_breakpoint_hit(&mut self) -> bool {
        self.breakpoint_hit.get_clear()
    }

    /// Reads the active breakpoints
    pub fn breakpoints(&self) -> &[Breakpoint] {
        &self.breakpoints
    }

    /// Reads the active breakpoints (mutable)
    pub fn breakpoints_mut(&mut self) -> &mut Vec<Breakpoint> {
        &mut self.breakpoints
    }

    /// Sets a breakpoint
    pub fn set_breakpoint(&mut self, bp: Breakpoint) {
        self.breakpoints.push(bp);
    }

    /// Clears a breakpoint
    pub fn clear_breakpoint(&mut self, bp: Breakpoint) {
        self.breakpoints.retain(|b| *b != bp);
    }

    /// Gets 'step over' target address from last instruction (if branched)
    pub fn get_step_over(&self) -> Option<Address> {
        self.step_over_addr
    }

    /// Configures instruction history
    pub fn enable_history(&mut self, val: bool) {
        self.history_enabled = val;
        self.history.clear();
        self.history_current = Default::default();
    }

    /// Configures system trap history
    pub fn enable_systrap_history(&mut self, val: bool) {
        self.systrap_history_enabled = val;
        self.systrap_history.clear();
    }

    /// Gets the instruction history, if enabled
    pub fn read_history(&mut self) -> Option<&[HistoryEntry]> {
        if self.history_enabled {
            Some(self.history.make_contiguous())
        } else {
            None
        }
    }

    /// Gets the systrap history, if enabled
    pub fn read_systrap_history(&mut self) -> Option<&[SystrapHistoryEntry]> {
        if self.systrap_history_enabled {
            Some(self.systrap_history.make_contiguous())
        } else {
            None
        }
    }

    /// Pumps the prefetch queue, unless it is already full
    pub(in crate::cpu_m68k) fn prefetch_pump(&mut self) -> Result<()> {
        if self.prefetch.len() >= 2 {
            return Ok(());
        }
        self.prefetch_pump_force()
    }

    /// Pumps a new word into the prefetch queue, regardless of current queue length
    fn prefetch_pump_force(&mut self) -> Result<()> {
        let fetch_addr = self.regs.pc.wrapping_add(4) & ADDRESS_MASK;

        let new_item = if CPU_TYPE >= M68020 && self.regs.cacr.e() {
            // We keep the tag full size so we can invalidate easily
            if fetch_addr & 1 != 0 {
                bail!("I-cache enabled but PC unaligned");
            }
            let cache_tag = fetch_addr & ICACHE_TAG_MASK;
            let cache_idx = ((fetch_addr & ICACHE_INDEX_MASK) >> 2) as usize;
            let cache_offset = (fetch_addr & ICACHE_OFFSET_MASK) as usize;
            if self.icache_tags[cache_idx] == cache_tag {
                // Cache hit
                self.history_current.icache_hit = true;
                self.advance_cycles(1)?;
                u16::from_be_bytes([
                    self.icache_lines[cache_idx][cache_offset],
                    self.icache_lines[cache_idx][cache_offset + 1],
                ])
            } else if !self.regs.cacr.f() {
                // Cache miss, fill cache line
                self.history_current.icache_miss = true;

                let addr = fetch_addr & !ICACHE_OFFSET_MASK;
                self.icache_lines[cache_idx] = self.read_ticks::<Long>(addr)?.to_be_bytes();
                self.icache_tags[cache_idx] = cache_tag;

                u16::from_be_bytes([
                    self.icache_lines[cache_idx][cache_offset],
                    self.icache_lines[cache_idx][cache_offset + 1],
                ])
            } else {
                // Cache miss and cache frozen, normal fetch
                self.history_current.icache_miss = true;
                self.read_ticks_program::<Word>(fetch_addr)?
            }
        } else {
            // Cache disabled/not available
            self.read_ticks_program::<Word>(fetch_addr)?
        };
        self.prefetch.push_back(new_item);
        self.regs.pc = (self.regs.pc + 2) & ADDRESS_MASK;
        Ok(())
    }

    /// Re-fills the prefetch queue
    pub fn prefetch_refill(&mut self) -> Result<()> {
        while self.prefetch.len() < 2 {
            self.prefetch_pump()?;
        }
        Ok(())
    }

    /// Fetches a 16-bit value, through the prefetch queue
    pub(in crate::cpu_m68k) fn fetch_pump(&mut self) -> Result<Word> {
        self.prefetch_pump_force()?;
        let v = self.prefetch.pop_front().unwrap();
        if self.history_enabled {
            self.history_current.push_raw(&v);
        }
        Ok(v)
    }

    /// Fetches a 16-bit value from prefetch queue
    pub(in crate::cpu_m68k) fn fetch(&mut self) -> Result<Word> {
        if self.prefetch.is_empty() {
            self.prefetch_pump()?;
        }
        let v = self.prefetch.pop_front().unwrap();
        if self.history_enabled {
            self.history_current.push_raw(&v);
        }
        Ok(v)
    }

    /// Executes a single CPU step.
    pub fn step(&mut self) -> Result<()> {
        debug_assert_eq!(self.prefetch.len(), 2);

        self.step_ea_addr = None;
        self.step_exception = false;
        self.step_over_addr = None;
        self.step_ea_load = None;

        if PMMU {
            // TODO this is incredibly expensive..
            self.restart_regs = Some(self.regs.clone());
        }

        // Flag exceptions before executing the instruction to act on them later
        let trace_exception = self.regs.sr.trace() && !self.trace_mask;
        let irq_exception = match self.bus.get_irq() {
            Some(7) => 7,
            Some(level) if level > self.regs.sr.int_prio_mask() => level,
            _ => 0,
        };

        // Start of instruction execution
        if self.history_enabled {
            self.history_current.pc = self.regs.pc;
            debug_assert!(self.history_current.raw.is_empty());
        }

        let start_cycles = self.cycles;
        let start_pc = self.regs.pc;
        let opcode = self.fetch()?;

        if self.decode_cache[opcode as usize].is_none() {
            let instr = Instruction::try_decode(CPU_TYPE, opcode);
            if instr.is_err() {
                if self.history_enabled {
                    self.history_current = Default::default();
                }
                debug!(
                    "Illegal instruction PC {:08X}: {:04X} {:016b} {}",
                    self.regs.pc,
                    opcode,
                    opcode,
                    instr.unwrap_err()
                );
                return self.raise_illegal_instruction();
            }

            self.decode_cache[opcode as usize] = Some(instr.unwrap());
        }

        if self.history_enabled {
            self.history_current.initial_regs = Some(self.regs.clone());
        }

        let instr = self.decode_cache[opcode as usize].clone().unwrap();
        let execute_result = self.execute_instruction(&instr);

        // Write history entry
        // This is done here already in case the instruction caused an
        // address error.
        if self.history_enabled {
            let mut entry = std::mem::take(&mut self.history_current);

            // Count prefetch reload towards the instruction
            if execute_result.is_ok() {
                self.prefetch_refill()?;
            }

            entry.cycles = self.cycles - start_cycles;
            entry.final_regs = Some(self.regs.clone());
            entry.ea = self.step_ea_addr;

            while self.history.len() >= Self::HISTORY_SIZE {
                self.history.pop_front();
            }
            self.history.push_back(HistoryEntry::Instruction(entry));
        }

        match execute_result {
            Ok(()) => {
                // Assert ea_commit() was called
                assert!(self.step_ea_load.is_none());
            }
            Err(e) => match e.downcast_ref() {
                Some(CpuError::AddressError(ae)) => {
                    let mut details = *ae;
                    details.ir = instr.data;
                    details.start_pc = start_pc;

                    debug!(
                        "Address error: read = {:?}, address = {:08X} PC = {:08X}",
                        details.read, details.address, self.regs.pc
                    );

                    self.raise_exception(
                        ExceptionGroup::Group0,
                        VECTOR_ADDRESS_ERROR,
                        Some(details),
                    )?;
                }
                Some(CpuError::BusError(ae)) => {
                    let mut details = *ae;
                    details.ir = instr.data;
                    details.start_pc = start_pc;
                    self.raise_exception(ExceptionGroup::Group0, VECTOR_BUS_ERROR, Some(details))?;
                }
                _ => {
                    bail!(
                        "PC: {:08X} Instruction: {:?} - error: {}",
                        start_pc,
                        instr,
                        e
                    );
                }
            },
        };

        self.prefetch_refill()?;

        // Check pending trace
        if trace_exception {
            self.raise_exception(ExceptionGroup::Group1, VECTOR_TRACE, None)?;
        }

        // Check pending interrupts
        if irq_exception != 0 {
            let level = irq_exception;
            if self
                .breakpoints
                .contains(&Breakpoint::InterruptLevel(level))
            {
                info!(
                    "Breakpoint hit (interrupt level): {}, PC: ${:08X}",
                    level, self.regs.pc
                );
                self.breakpoint_hit.set();
            }

            self.raise_irq(
                level,
                VECTOR_AUTOVECTOR_OFFSET + (Address::from(level - 1) * 4),
            )?;
        }

        // Test breakpoint on next PC location
        if self
            .breakpoints
            .contains(&Breakpoint::Execution(self.regs.pc))
        {
            info!("Breakpoint hit (execution): ${:08X}", self.regs.pc);
            self.breakpoint_hit.set();
        }
        if self
            .breakpoints
            .contains(&Breakpoint::StepOver(self.regs.pc))
        {
            self.breakpoint_hit.set();
            self.clear_breakpoint(Breakpoint::StepOver(self.regs.pc));
        }

        Ok(())
    }

    /// Tests if we should stop due to 'step out' debugger action
    fn test_step_out(&mut self) {
        let mut bp_hit = false;
        let sp = self.regs.read_a::<Address>(7);
        self.breakpoints.retain(|bp| {
            if let Breakpoint::StepOut(addr) = bp {
                if *addr < sp {
                    bp_hit = true;
                    false
                } else {
                    true
                }
            } else {
                true
            }
        });
        if bp_hit {
            self.breakpoint_hit.set();
        }
    }

    /// Advances by the given amount of cycles
    pub(in crate::cpu_m68k) fn advance_cycles(&mut self, ticks: Ticks) -> Result<()> {
        for _ in 0..ticks {
            self.cycles += 1;
            self.bus.tick(1)?;
        }
        Ok(())
    }

    /// Sets the program counter and flushes the prefetch queue
    pub fn set_pc(&mut self, pc: Address) -> Result<()> {
        self.prefetch.clear();
        self.regs.pc = pc.wrapping_sub(4) & ADDRESS_MASK;
        Ok(())
    }

    /// Sets SR, masking CPU model dependent bits accordingly
    pub fn set_sr(&mut self, sr: Word) {
        self.regs.sr.set_sr(match CPU_TYPE {
            M68000 => sr & M68000_SR_MASK,
            M68020 => sr & M68020_SR_MASK,
            _ => unreachable!(),
        });
    }

    /// Gets the location where the next fetch() would occur from,
    /// regardless of the prefetch queue.
    fn get_fetch_addr(&self) -> Address {
        let prefetch_offset = (2 - self.prefetch.len()) as Address;
        self.regs.pc.wrapping_add(prefetch_offset * 2) & ADDRESS_MASK
    }

    /// Raises an illegal instruction exception
    fn raise_illegal_instruction(&mut self) -> Result<()> {
        warn!("Illegal instruction at PC ${:08X}", self.regs.pc);
        self.advance_cycles(4)?;
        self.raise_exception(ExceptionGroup::Group1, VECTOR_ILLEGAL, None)?;
        Ok(())
    }

    /// Raises a privilege violation exception
    fn raise_privilege_violation(&mut self) -> Result<()> {
        self.advance_cycles(4)?;
        self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None)?;
        Ok(())
    }

    /// Raises an IRQ to be executed next
    fn raise_irq(&mut self, level: u8, vector: Address) -> Result<()> {
        let start_cycles = self.cycles;
        let saved_sr = self.regs.sr.sr();

        // Resume in supervisor mode
        self.regs.sr.set_supervisor(true);
        self.regs.sr.set_trace(false);

        // Update mask
        self.regs.sr.set_int_prio_mask(level);

        match CPU_TYPE {
            M68000 => {
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(6);

                self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;

                // 6 cycles idle
                self.advance_cycles(6)?;
                // Interrupt ack
                self.advance_cycles(4)?;
                // 4 cycles idle
                self.advance_cycles(4)?;

                self.write_ticks(self.regs.ssp().wrapping_add(4), self.regs.pc as u16)?;
                self.write_ticks(self.regs.ssp().wrapping_add(2), (self.regs.pc >> 16) as u16)?;
            }
            #[allow(clippy::identity_op)]
            _ => {
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(8);
                // 6 cycles idle, interrupt ack, 4 cycles idle
                self.advance_cycles(6 + 4 + 4)?;

                self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                self.write_ticks(self.regs.ssp().wrapping_add(2), (self.regs.pc >> 16) as u16)?;
                self.write_ticks(self.regs.ssp().wrapping_add(4), self.regs.pc as u16)?;
                self.write_ticks(
                    self.regs.ssp().wrapping_add(6),
                    0b0000_0000_0000_0000u16 | (vector as u16),
                )?;
            }
        };

        if self
            .breakpoints
            .contains(&Breakpoint::ExceptionVector(vector))
        {
            info!(
                "Breakpoint hit (exception vector): {:08X}, PC: ${:08X}",
                vector, self.regs.pc
            );
            self.breakpoint_hit.set();
        }

        // Jump to vector
        let vector_base = if CPU_TYPE >= M68010 { self.regs.vbr } else { 0 };
        let new_pc = self.read_ticks::<Long>(vector_base.wrapping_add(vector))?;
        self.set_pc(new_pc)?;
        self.prefetch_pump()?;
        self.advance_cycles(2)?; // 2x idle
        self.prefetch_pump()?;

        if self.history_enabled {
            self.history.push_back(HistoryEntry::Exception {
                vector,
                cycles: self.cycles - start_cycles,
            });
        }

        Ok(())
    }

    /// Raises a CPU exception in supervisor mode.
    pub(in crate::cpu_m68k) fn raise_exception(
        &mut self,
        group: ExceptionGroup,
        vector: Address,
        details: Option<Group0Details>,
    ) -> Result<()> {
        let start_cycles = self.cycles;
        let mut saved_sr = self.regs.sr.sr();

        // Resume in supervisor mode
        self.regs.sr.set_supervisor(true);
        self.regs.sr.set_trace(false);

        // Write exception stack frame
        match group {
            ExceptionGroup::Group0 => {
                self.step_exception = true;

                self.advance_cycles(8)?; // idle
                let details = details.expect("Address error details not passed");

                match CPU_TYPE {
                    M68000 => {
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(14);
                        self.write_ticks(self.regs.ssp().wrapping_add(12), self.regs.pc as u16)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(8), saved_sr)?;
                        self.write_ticks(
                            self.regs.ssp().wrapping_add(10),
                            (self.regs.pc >> 16) as u16,
                        )?;
                        self.write_ticks(self.regs.ssp().wrapping_add(6), details.ir)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(4), details.address as u16)?;
                        // Function code (3), I/N (1), R/W (1)
                        // TODO I/N, function code..
                        self.write_ticks(
                            self.regs.ssp().wrapping_add(0),
                            if details.read { 1_u16 << 4 } else { 0_u16 },
                        )?;
                        self.write_ticks(
                            self.regs.ssp().wrapping_add(2),
                            (details.address >> 16) as u16,
                        )?;
                    }
                    _ => {
                        if let Some(regs) = self.restart_regs.take() {
                            saved_sr = regs.sr.sr();
                            self.regs = regs;
                            self.regs.sr.set_supervisor(true);
                            self.regs.sr.set_trace(false);
                        } else {
                            log::error!("Cannot reset registers for stacking a bus error frame");
                        }

                        if self.regs.pc == details.start_pc && self.prefetch.len() == 2 {
                            // Bus error at instruction boundary
                            *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(32);
                            self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                            self.write_ticks(self.regs.ssp().wrapping_add(0x02), details.start_pc)?;
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x06),
                                0b1010_0000_0000_0000 | (vector as u16),
                            )?;
                            // Internal register
                            self.write_ticks(self.regs.ssp().wrapping_add(0x08), 0u16)?;
                            // Special status register
                            // TODO size
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x0A),
                                *Group0Ssw::default()
                                    .with_read(details.read)
                                    .with_df(true)
                                    .with_function_code(details.function_code)
                                    .with_size(match details.size {
                                        1 => 1,
                                        2 => 2,
                                        4 => 0,
                                        _ => {
                                            log::error!(
                                                "Unknown size in group 0 details: {}",
                                                details.size
                                            );
                                            // Assume long
                                            0
                                        }
                                    }),
                            )?;
                            // Instruction pipe stage C
                            self.write_ticks(self.regs.ssp().wrapping_add(0x0C), 0u16)?;
                            // Instruction pipe stage B
                            self.write_ticks(self.regs.ssp().wrapping_add(0x0E), 0u16)?;
                            // Data cycle fault address
                            self.write_ticks(self.regs.ssp().wrapping_add(0x10), details.address)?;
                            // Internal registers
                            self.write_ticks(self.regs.ssp().wrapping_add(0x14), 0u32)?;
                            // Data output buffer
                            self.write_ticks(self.regs.ssp().wrapping_add(0x18), 0u32)?;
                            // Internal registers
                            self.write_ticks(self.regs.ssp().wrapping_add(0x1C), 0u32)?;
                        } else {
                            *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(92);
                            self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                            self.write_ticks(self.regs.ssp().wrapping_add(0x02), details.start_pc)?;
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x06),
                                0b1011_0000_0000_0000 | (vector as u16),
                            )?;
                            // Internal register
                            self.write_ticks(self.regs.ssp().wrapping_add(0x08), 0u16)?;
                            // Special status register
                            // TODO size
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x0A),
                                *Group0Ssw::default()
                                    .with_read(details.read)
                                    .with_df(true)
                                    .with_function_code(details.function_code)
                                    .with_size(match details.size {
                                        1 => 1,
                                        2 => 2,
                                        4 => 0,
                                        _ => {
                                            log::error!(
                                                "Unknown size in group 0 details: {}",
                                                details.size
                                            );
                                            // Assume long
                                            0
                                        }
                                    }),
                            )?;
                            // Instruction pipe stage C
                            self.write_ticks(self.regs.ssp().wrapping_add(0x0C), 0u16)?;
                            // Instruction pipe stage B
                            self.write_ticks(self.regs.ssp().wrapping_add(0x0E), 0u16)?;
                            // Data cycle fault address
                            self.write_ticks(self.regs.ssp().wrapping_add(0x10), details.address)?;

                            // More internal stuff
                            for i in (0x14..=0x5A).step_by(2) {
                                self.write_ticks(self.regs.ssp().wrapping_add(i), 0u16)?;
                            }
                        }
                    }
                }
            }
            ExceptionGroup::Group1 | ExceptionGroup::Group2 => {
                //debug!(
                //    "Exception {:?}, vector {:08X} @  PC = {:08X}",
                //    group, vector, self.regs.pc
                //);

                let pc = self.regs.pc;
                match CPU_TYPE {
                    M68000 => {
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(6);
                        self.write_ticks(self.regs.ssp().wrapping_add(4), pc as u16)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(2), (pc >> 16) as u16)?;
                    }
                    _ => {
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(8);
                        self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(2), (pc >> 16) as u16)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(4), pc as u16)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(6), vector as u16)?;
                    }
                }
            }
        }

        if self
            .breakpoints
            .contains(&Breakpoint::ExceptionVector(vector))
        {
            info!(
                "Breakpoint hit (exception vector): {:08X}, PC: ${:08X}",
                vector, self.regs.pc
            );
            self.breakpoint_hit.set();
        }

        let vector_base = if CPU_TYPE >= M68010 { self.regs.vbr } else { 0 };
        let new_pc = self.read_ticks::<Long>(vector_base.wrapping_add(vector))?;
        self.set_pc(new_pc)?;
        self.prefetch_pump()?;
        self.advance_cycles(2)?; // 2x idle
        self.prefetch_pump()?;

        if self.history_enabled {
            self.history.push_back(HistoryEntry::Exception {
                vector,
                cycles: self.cycles - start_cycles,
            });
        }

        Ok(())
    }

    /// Executes a previously decoded instruction.
    fn execute_instruction(&mut self, instr: &Instruction) -> Result<()> {
        match instr.mnemonic {
            InstructionMnemonic::AND_l => self.op_bitwise::<Long>(instr, |a, b| a & b),
            InstructionMnemonic::AND_w => self.op_bitwise::<Word>(instr, |a, b| a & b),
            InstructionMnemonic::AND_b => self.op_bitwise::<Byte>(instr, |a, b| a & b),
            InstructionMnemonic::ANDI_l => self.op_bitwise_immediate::<Long>(instr, |a, b| a & b),
            InstructionMnemonic::ANDI_w => self.op_bitwise_immediate::<Word>(instr, |a, b| a & b),
            InstructionMnemonic::ANDI_b => self.op_bitwise_immediate::<Byte>(instr, |a, b| a & b),
            InstructionMnemonic::ANDI_ccr => self.op_bitwise_ccr(instr, |a, b| a & b),
            InstructionMnemonic::ANDI_sr => self.op_bitwise_sr(instr, |a, b| a & b),
            InstructionMnemonic::EOR_l => self.op_bitwise::<Long>(instr, |a, b| a ^ b),
            InstructionMnemonic::EOR_w => self.op_bitwise::<Word>(instr, |a, b| a ^ b),
            InstructionMnemonic::EOR_b => self.op_bitwise::<Byte>(instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_l => self.op_bitwise_immediate::<Long>(instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_w => self.op_bitwise_immediate::<Word>(instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_b => self.op_bitwise_immediate::<Byte>(instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_ccr => self.op_bitwise_ccr(instr, |a, b| a ^ b),
            InstructionMnemonic::EORI_sr => self.op_bitwise_sr(instr, |a, b| a ^ b),
            InstructionMnemonic::OR_l => self.op_bitwise::<Long>(instr, |a, b| a | b),
            InstructionMnemonic::OR_w => self.op_bitwise::<Word>(instr, |a, b| a | b),
            InstructionMnemonic::OR_b => self.op_bitwise::<Byte>(instr, |a, b| a | b),
            InstructionMnemonic::ORI_l => self.op_bitwise_immediate::<Long>(instr, |a, b| a | b),
            InstructionMnemonic::ORI_w => self.op_bitwise_immediate::<Word>(instr, |a, b| a | b),
            InstructionMnemonic::ORI_b => self.op_bitwise_immediate::<Byte>(instr, |a, b| a | b),
            InstructionMnemonic::ORI_ccr => self.op_bitwise_ccr(instr, |a, b| a | b),
            InstructionMnemonic::ORI_sr => self.op_bitwise_sr(instr, |a, b| a | b),
            InstructionMnemonic::SUB_l => self.op_alu::<Long>(instr, Self::alu_sub),
            InstructionMnemonic::SUB_w => self.op_alu::<Word>(instr, Self::alu_sub),
            InstructionMnemonic::SUB_b => self.op_alu::<Byte>(instr, Self::alu_sub),
            InstructionMnemonic::SUBA_l => self.op_alu_a::<Long>(instr, Self::alu_sub),
            InstructionMnemonic::SUBA_w => self.op_alu_a::<Word>(instr, Self::alu_sub),
            InstructionMnemonic::SUBI_l => self.op_alu_immediate::<Long>(instr, Self::alu_sub),
            InstructionMnemonic::SUBI_w => self.op_alu_immediate::<Word>(instr, Self::alu_sub),
            InstructionMnemonic::SUBI_b => self.op_alu_immediate::<Byte>(instr, Self::alu_sub),
            InstructionMnemonic::SUBQ_l => self.op_alu_quick::<Long>(instr, Self::alu_sub),
            InstructionMnemonic::SUBQ_w => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    // A word operation on an address register affects the entire 32-bit address.
                    self.op_alu_quick::<Long>(instr, Self::alu_sub)
                } else {
                    self.op_alu_quick::<Word>(instr, Self::alu_sub)
                }
            }
            InstructionMnemonic::SUBQ_b => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    return self.raise_illegal_instruction();
                }
                self.op_alu_quick::<Byte>(instr, Self::alu_sub)
            }
            InstructionMnemonic::SUBX_l => self.op_alu_x::<Long>(instr, Self::alu_sub_x),
            InstructionMnemonic::SUBX_w => self.op_alu_x::<Word>(instr, Self::alu_sub_x),
            InstructionMnemonic::SUBX_b => self.op_alu_x::<Byte>(instr, Self::alu_sub_x),
            InstructionMnemonic::ADD_l => self.op_alu::<Long>(instr, Self::alu_add),
            InstructionMnemonic::ADD_w => self.op_alu::<Word>(instr, Self::alu_add),
            InstructionMnemonic::ADD_b => self.op_alu::<Byte>(instr, Self::alu_add),
            InstructionMnemonic::ADDA_l => self.op_alu_a::<Long>(instr, Self::alu_add),
            InstructionMnemonic::ADDA_w => self.op_alu_a::<Word>(instr, Self::alu_add),
            InstructionMnemonic::ADDI_l => self.op_alu_immediate::<Long>(instr, Self::alu_add),
            InstructionMnemonic::ADDI_w => self.op_alu_immediate::<Word>(instr, Self::alu_add),
            InstructionMnemonic::ADDI_b => self.op_alu_immediate::<Byte>(instr, Self::alu_add),
            InstructionMnemonic::ADDQ_l => self.op_alu_quick::<Long>(instr, Self::alu_add),
            InstructionMnemonic::ADDQ_w => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    // A word operation on an address register affects the entire 32-bit address.
                    self.op_alu_quick::<Long>(instr, Self::alu_add)
                } else {
                    self.op_alu_quick::<Word>(instr, Self::alu_add)
                }
            }
            InstructionMnemonic::ADDQ_b => {
                if instr.get_addr_mode()? == AddressingMode::AddressRegister {
                    return self.raise_illegal_instruction();
                }
                self.op_alu_quick::<Byte>(instr, Self::alu_add)
            }
            InstructionMnemonic::ADDX_l => self.op_alu_x::<Long>(instr, Self::alu_add_x),
            InstructionMnemonic::ADDX_w => self.op_alu_x::<Word>(instr, Self::alu_add_x),
            InstructionMnemonic::ADDX_b => self.op_alu_x::<Byte>(instr, Self::alu_add_x),
            InstructionMnemonic::CMP_l => self.op_cmp::<Long>(instr),
            InstructionMnemonic::CMP_w => self.op_cmp::<Word>(instr),
            InstructionMnemonic::CMP_b => self.op_cmp::<Byte>(instr),
            InstructionMnemonic::CMPA_l => self.op_cmp_address::<Long>(instr),
            InstructionMnemonic::CMPA_w => self.op_cmp_address::<Word>(instr),
            InstructionMnemonic::CMPI_l => self.op_cmp_immediate::<Long>(instr),
            InstructionMnemonic::CMPI_w => self.op_cmp_immediate::<Word>(instr),
            InstructionMnemonic::CMPI_b => self.op_cmp_immediate::<Byte>(instr),
            InstructionMnemonic::CMPM_l => self.op_cmpm::<Long>(instr),
            InstructionMnemonic::CMPM_w => self.op_cmpm::<Word>(instr),
            InstructionMnemonic::CMPM_b => self.op_cmpm::<Byte>(instr),
            InstructionMnemonic::MULU_w => self.op_mulu(instr),
            InstructionMnemonic::MULS_w => self.op_muls_w(instr),
            InstructionMnemonic::DIVU_w => self.op_divu(instr),
            InstructionMnemonic::DIVS_w => self.op_divs(instr),
            InstructionMnemonic::NOP => Ok(()),
            InstructionMnemonic::SWAP => self.op_swap(instr),
            InstructionMnemonic::TRAP => self.op_trap(instr),
            InstructionMnemonic::BTST_imm => self.op_bit::<true>(instr, None),
            InstructionMnemonic::BSET_imm => self.op_bit::<true>(instr, Some(|v, bit| v | bit)),
            InstructionMnemonic::BCLR_imm => self.op_bit::<true>(instr, Some(|v, bit| v & !bit)),
            InstructionMnemonic::BCHG_imm => self.op_bit::<true>(instr, Some(|v, bit| v ^ bit)),
            InstructionMnemonic::BTST_dn => self.op_bit::<false>(instr, None),
            InstructionMnemonic::BSET_dn => self.op_bit::<false>(instr, Some(|v, bit| v | bit)),
            InstructionMnemonic::BCLR_dn => self.op_bit::<false>(instr, Some(|v, bit| v & !bit)),
            InstructionMnemonic::BCHG_dn => self.op_bit::<false>(instr, Some(|v, bit| v ^ bit)),
            InstructionMnemonic::MOVEP_w => self.op_movep::<2, Word>(instr),
            InstructionMnemonic::MOVEP_l => self.op_movep::<4, Long>(instr),
            InstructionMnemonic::MOVEA_l => self.op_movea::<Long>(instr),
            InstructionMnemonic::MOVEA_w => self.op_movea::<Word>(instr),
            InstructionMnemonic::MOVE_l => self.op_move::<Long>(instr),
            InstructionMnemonic::MOVE_w => self.op_move::<Word>(instr),
            InstructionMnemonic::MOVE_b => self.op_move::<Byte>(instr),
            InstructionMnemonic::MOVEfromSR => self.op_move_from_sr(instr),
            InstructionMnemonic::MOVEtoSR => self.op_move_to_sr(instr),
            InstructionMnemonic::MOVEtoCCR => self.op_move_to_ccr(instr),
            InstructionMnemonic::MOVEtoUSP => self.op_move_to_usp(instr),
            InstructionMnemonic::MOVEfromUSP => self.op_move_from_usp(instr),
            InstructionMnemonic::NEG_l => self.op_alu_zero::<Long>(instr, Self::alu_sub),
            InstructionMnemonic::NEG_w => self.op_alu_zero::<Word>(instr, Self::alu_sub),
            InstructionMnemonic::NEG_b => self.op_alu_zero::<Byte>(instr, Self::alu_sub),
            InstructionMnemonic::NEGX_l => self.op_alu_zero::<Long>(instr, Self::alu_sub_x),
            InstructionMnemonic::NEGX_w => self.op_alu_zero::<Word>(instr, Self::alu_sub_x),
            InstructionMnemonic::NEGX_b => self.op_alu_zero::<Byte>(instr, Self::alu_sub_x),
            InstructionMnemonic::CLR_l => self.op_clr::<Long>(instr),
            InstructionMnemonic::CLR_w => self.op_clr::<Word>(instr),
            InstructionMnemonic::CLR_b => self.op_clr::<Byte>(instr),
            InstructionMnemonic::NOT_l => self.op_not::<Long>(instr),
            InstructionMnemonic::NOT_w => self.op_not::<Word>(instr),
            InstructionMnemonic::NOT_b => self.op_not::<Byte>(instr),
            InstructionMnemonic::EXT_l => self.op_ext::<Long, Word>(instr),
            InstructionMnemonic::EXT_w => self.op_ext::<Word, Byte>(instr),
            InstructionMnemonic::EXTB_l => self.op_ext::<Long, Byte>(instr),
            InstructionMnemonic::SBCD => self.op_sbcd(instr),
            InstructionMnemonic::NBCD => self.op_nbcd(instr),
            InstructionMnemonic::ABCD => self.op_abcd(instr),
            InstructionMnemonic::PEA => self.op_lea_pea(instr),
            InstructionMnemonic::LEA => self.op_lea_pea(instr),
            InstructionMnemonic::ILLEGAL => self.raise_illegal_instruction(),
            InstructionMnemonic::TAS => self.op_tas(instr),
            InstructionMnemonic::TST_b => self.op_tst::<Byte>(instr),
            InstructionMnemonic::TST_w => self.op_tst::<Word>(instr),
            InstructionMnemonic::TST_l => self.op_tst::<Long>(instr),
            InstructionMnemonic::LINK_w => self.op_link_w(instr),
            InstructionMnemonic::LINK_l => self.op_link_l(instr),
            InstructionMnemonic::UNLINK => self.op_unlink(instr),
            InstructionMnemonic::RESET => self.op_reset(instr),
            InstructionMnemonic::RTE => self.op_rte(instr),
            InstructionMnemonic::RTS => self.op_rts(instr),
            InstructionMnemonic::RTR => self.op_rtr(instr),
            InstructionMnemonic::STOP => bail!("STOP instruction encountered"),
            InstructionMnemonic::TRAPV => self.op_trapv(instr),
            InstructionMnemonic::JSR => self.op_jmp_jsr(instr),
            InstructionMnemonic::JMP => self.op_jmp_jsr(instr),
            InstructionMnemonic::MOVEM_mem_l => self.op_movem_mem::<Long>(instr),
            InstructionMnemonic::MOVEM_mem_w => self.op_movem_mem::<Word>(instr),
            InstructionMnemonic::MOVEM_reg_l => self.op_movem_reg::<Long>(instr),
            InstructionMnemonic::MOVEM_reg_w => self.op_movem_reg::<Word>(instr),
            InstructionMnemonic::CHK_w => self.op_chk::<Word>(instr),
            InstructionMnemonic::Scc => self.op_scc(instr),
            InstructionMnemonic::DBcc => self.op_dbcc(instr),
            InstructionMnemonic::Bcc => self.op_bcc::<false>(instr),
            InstructionMnemonic::BSR => self.op_bcc::<true>(instr),
            InstructionMnemonic::MOVEQ => self.op_moveq(instr),
            InstructionMnemonic::EXG => self.op_exg(instr),
            InstructionMnemonic::ASL_b => self.op_shrot::<Byte>(instr, Self::alu_asl),
            InstructionMnemonic::ASL_w => self.op_shrot::<Word>(instr, Self::alu_asl),
            InstructionMnemonic::ASL_l => self.op_shrot::<Long>(instr, Self::alu_asl),
            InstructionMnemonic::ASR_b => self.op_shrot::<Byte>(instr, Self::alu_asr),
            InstructionMnemonic::ASR_w => self.op_shrot::<Word>(instr, Self::alu_asr),
            InstructionMnemonic::ASR_l => self.op_shrot::<Long>(instr, Self::alu_asr),
            InstructionMnemonic::ASL_ea => self.op_shrot_ea(instr, Self::alu_asl),
            InstructionMnemonic::ASR_ea => self.op_shrot_ea(instr, Self::alu_asr),
            InstructionMnemonic::LSL_b => self.op_shrot::<Byte>(instr, Self::alu_lsl),
            InstructionMnemonic::LSL_w => self.op_shrot::<Word>(instr, Self::alu_lsl),
            InstructionMnemonic::LSL_l => self.op_shrot::<Long>(instr, Self::alu_lsl),
            InstructionMnemonic::LSR_b => self.op_shrot::<Byte>(instr, Self::alu_lsr),
            InstructionMnemonic::LSR_w => self.op_shrot::<Word>(instr, Self::alu_lsr),
            InstructionMnemonic::LSR_l => self.op_shrot::<Long>(instr, Self::alu_lsr),
            InstructionMnemonic::LSL_ea => self.op_shrot_ea(instr, Self::alu_lsl),
            InstructionMnemonic::LSR_ea => self.op_shrot_ea(instr, Self::alu_lsr),
            InstructionMnemonic::ROXL_b => self.op_shrot::<Byte>(instr, Self::alu_roxl),
            InstructionMnemonic::ROXL_w => self.op_shrot::<Word>(instr, Self::alu_roxl),
            InstructionMnemonic::ROXL_l => self.op_shrot::<Long>(instr, Self::alu_roxl),
            InstructionMnemonic::ROXR_b => self.op_shrot::<Byte>(instr, Self::alu_roxr),
            InstructionMnemonic::ROXR_w => self.op_shrot::<Word>(instr, Self::alu_roxr),
            InstructionMnemonic::ROXR_l => self.op_shrot::<Long>(instr, Self::alu_roxr),
            InstructionMnemonic::ROXL_ea => self.op_shrot_ea(instr, Self::alu_roxl),
            InstructionMnemonic::ROXR_ea => self.op_shrot_ea(instr, Self::alu_roxr),
            InstructionMnemonic::ROL_b => self.op_shrot::<Byte>(instr, Self::alu_rol),
            InstructionMnemonic::ROL_w => self.op_shrot::<Word>(instr, Self::alu_rol),
            InstructionMnemonic::ROL_l => self.op_shrot::<Long>(instr, Self::alu_rol),
            InstructionMnemonic::ROR_b => self.op_shrot::<Byte>(instr, Self::alu_ror),
            InstructionMnemonic::ROR_w => self.op_shrot::<Word>(instr, Self::alu_ror),
            InstructionMnemonic::ROR_l => self.op_shrot::<Long>(instr, Self::alu_ror),
            InstructionMnemonic::ROL_ea => self.op_shrot_ea(instr, Self::alu_rol),
            InstructionMnemonic::ROR_ea => self.op_shrot_ea(instr, Self::alu_ror),
            InstructionMnemonic::LINEA => {
                if self.breakpoints.contains(&Breakpoint::LineA(instr.data)) {
                    info!(
                        "Breakpoint hit (LINEA): ${:04X}, PC: ${:08X}",
                        instr.data, self.regs.pc
                    );
                    self.breakpoint_hit.set();
                }

                if self.systrap_history_enabled {
                    while self.systrap_history.len() >= Self::HISTORY_SIZE {
                        self.systrap_history.pop_front();
                    }
                    self.systrap_history.push_back(SystrapHistoryEntry {
                        trap: instr.data,
                        cycles: self.cycles,
                        pc: self.regs.pc,
                    });
                }

                self.advance_cycles(4)?;
                self.raise_exception(ExceptionGroup::Group2, VECTOR_LINEA, None)
            }
            InstructionMnemonic::LINEF => self.op_linef(instr),

            // M68010 ------------------------------------------------------------------------------
            InstructionMnemonic::MOVEC_l => self.op_movec(instr),
            InstructionMnemonic::RTD => self.op_rtd(instr),
            InstructionMnemonic::MOVEfromCCR => self.op_move_from_ccr(instr),

            // M68020 ------------------------------------------------------------------------------
            InstructionMnemonic::BFCLR => self.op_bfclr(instr),
            InstructionMnemonic::BFCHG => self.op_bfchg(instr),
            InstructionMnemonic::BFFFO => self.op_bfffo(instr),
            InstructionMnemonic::BFEXTU => self.op_bfextu(instr),
            InstructionMnemonic::BFEXTS => self.op_bfexts(instr),
            InstructionMnemonic::BFINS => self.op_bfins(instr),
            InstructionMnemonic::BFSET => self.op_bfset(instr),
            InstructionMnemonic::BFTST => self.op_bftst(instr),
            InstructionMnemonic::MULx_l => self.op_mulx_l(instr),
            InstructionMnemonic::DIVx_l => self.op_divx_l(instr),
            InstructionMnemonic::CHK_l => self.op_chk::<Long>(instr),
            InstructionMnemonic::CAS_b => self.op_cas::<Byte>(instr),
            InstructionMnemonic::CAS_w => self.op_cas::<Word>(instr),
            InstructionMnemonic::CAS_l => self.op_cas::<Long>(instr),

            // FPU ---------------------------------------------------------------------------------
            InstructionMnemonic::FNOP => self.op_fnop(instr),
            InstructionMnemonic::FSAVE => self.op_fsave(instr),
            InstructionMnemonic::FRESTORE => self.op_frestore(instr),
            InstructionMnemonic::FOP_000 => self.op_f000(instr),
            InstructionMnemonic::FBcc_l => self.op_fbcc::<true>(instr),
            InstructionMnemonic::FBcc_w => self.op_fbcc::<false>(instr),
            InstructionMnemonic::FScc_b => self.op_fscc(instr),

            // PMMU --------------------------------------------------------------------------------
            InstructionMnemonic::POP_000 => self.op_pop_000(instr),
        }
    }

    /// LINEF
    pub(in crate::cpu_m68k) fn op_linef(&mut self, instr: &Instruction) -> Result<()> {
        if self.breakpoints.contains(&Breakpoint::LineF(instr.data)) {
            info!(
                "Breakpoint hit (LINEF): ${:04X}, PC: ${:08X}",
                instr.data, self.regs.pc
            );
            self.breakpoint_hit.set();
        }

        self.advance_cycles(4)?;
        self.raise_exception(ExceptionGroup::Group2, VECTOR_LINEF, None)
    }

    /// SWAP
    fn op_swap(&mut self, instr: &Instruction) -> Result<()> {
        let v: Long = self.regs.read_d(instr.get_op2());
        let result = v.rotate_left(16);

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
                self.advance_cycles(4)?;
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
        // This has to be a fetch from the prefetch queue for PC to stay correct
        // in the exception frame.
        let a = self.fetch()?;

        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        // Now load a
        self.prefetch_pump()?;
        let b = self.regs.sr.sr();
        self.set_sr(calcfn(a, b));

        // Idle cycles and dummy read
        self.advance_cycles(8)?;
        self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        self.prefetch_pump()?;

        Ok(())
    }

    /// TRAP
    fn op_trap(&mut self, instr: &Instruction) -> Result<()> {
        // Offset PC correctly for exception stack frame for TRAP
        self.regs.pc = self.regs.pc.wrapping_add(2);

        self.advance_cycles(4)?;
        self.raise_exception(
            ExceptionGroup::Group2,
            instr.trap_get_vector() * 4 + VECTOR_TRAP_OFFSET,
            None,
        )
    }

    /// TRAPV
    fn op_trapv(&mut self, _instr: &Instruction) -> Result<()> {
        self.prefetch_pump()?;

        if !self.regs.sr.v() {
            return Ok(());
        }

        self.raise_exception(ExceptionGroup::Group2, VECTOR_TRAPV, None)
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
                self.advance_cycles(4)?;
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
            self.regs.sr.set_ccr(ccr);
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
                self.regs.write_d(instr.get_op1(), result);
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
                self.write_ticks(b_addr, result)?;
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
                self.advance_cycles(2)?;
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

    /// MULS (Word)
    fn op_muls_w(&mut self, instr: &Instruction) -> Result<()> {
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
            for (i, d) in data.iter_mut().enumerate().take(N) {
                let b_addr = addr.wrapping_add((i * 2) as Address);
                *d = self.read_ticks::<Byte>(b_addr)?;
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
        let value: T =
            self.read_ea_with::<T, false>(instr, instr.get_addr_mode()?, instr.get_op2())?;

        self.regs.sr.set_z(value == T::zero());
        self.regs.sr.set_n(value & T::msb() != T::zero());
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);

        // Clear EA cache to write to the left mode
        self.step_ea_addr = None;
        instr.clear_extword();

        match (
            std::mem::size_of::<T>(),
            instr.get_addr_mode_left()?,
            instr.get_addr_mode()?,
        ) {
            (4, AddressingMode::IndirectPreDec, _) => {
                // Writes high to low and fetch instead of idle cycles
                let addr = self.regs.read_a_predec::<Address>(instr.get_op1(), 4);
                self.prefetch_pump()?;
                self.write_ticks_order::<T, TORDER_HIGHLOW>(addr, value)?;
            }
            (_, AddressingMode::IndirectPreDec, _) => {
                // MOVE ..., -(An) this mode has a fetch instead of the idle cycles.
                let addr: Address = self
                    .regs
                    .read_a_predec(instr.get_op1(), std::mem::size_of::<T>());
                self.prefetch_pump()?;
                self.write_ticks(addr, value)?;
            }
            (
                _,
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
                self.write_ea_with::<T, false, TORDER_LOWHIGH>(
                    instr,
                    instr.get_addr_mode_left()?,
                    instr.get_op1(),
                    value,
                )?;
            }
            _ => self.write_ea_with::<T, false, TORDER_LOWHIGH>(
                instr,
                instr.get_addr_mode_left()?,
                instr.get_op1(),
                value,
            )?,
        }

        Ok(())
    }

    /// MOVEfromSR
    fn op_move_from_sr(&mut self, instr: &Instruction) -> Result<()> {
        if CPU_TYPE >= M68010 && !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        let value = self.regs.sr.sr();

        // Discarded read, prefetch
        self.read_ea::<Word>(instr, instr.get_op2())?;
        self.prefetch_pump()?;

        self.write_ea(instr, instr.get_op2(), value)?;

        // Idle cycles
        match instr.get_addr_mode()? {
            AddressingMode::DataRegister | AddressingMode::AddressRegister => {
                self.advance_cycles(2)?;
            }
            _ => (),
        }

        Ok(())
    }

    /// MOVEtoSR
    fn op_move_to_sr(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }
        let value: Word = self.read_ea(instr, instr.get_op2())?;

        // Idle cycles and discarded read
        self.advance_cycles(4)?;
        self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;

        self.set_sr(value);
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

    /// MOVEfromCCR
    fn op_move_from_ccr(&mut self, instr: &Instruction) -> Result<()> {
        self.advance_cycles(4)?;
        self.write_ea::<Word>(instr, instr.get_op2(), self.regs.sr.ccr().into())?;
        self.prefetch_pump()?;

        Ok(())
    }

    /// MOVEtoUSP
    fn op_move_to_usp(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }
        let value: Address = self.regs.read_a(instr.get_op2());

        self.regs.usp = value;
        Ok(())
    }

    /// MOVEfromUSP
    fn op_move_from_usp(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
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
                    self.advance_cycles(2)?;
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
                    self.advance_cycles(2)?;
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
        self.op_alu_x::<Byte>(instr, Self::alu_sub_bcd)?;
        if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// ABCD
    fn op_abcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_x::<Byte>(instr, Self::alu_add_bcd)?;
        if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// NBCD
    fn op_nbcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_zero::<Byte>(instr, Self::alu_sub_bcd)?;
        if instr.get_addr_mode()? == AddressingMode::DataRegister {
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// LEA/PEA
    fn op_lea_pea(&mut self, instr: &Instruction) -> Result<()> {
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

        match instr.mnemonic {
            InstructionMnemonic::LEA => self.regs.write_a(instr.get_op1(), value),
            InstructionMnemonic::PEA => {
                // Push to stack
                let addr = self.regs.read_a_predec(7, std::mem::size_of::<Long>());
                self.write_ticks(addr, value)?;
            }
            _ => unreachable!(),
        }
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

    /// LINK.w
    fn op_link_w(&mut self, instr: &Instruction) -> Result<()> {
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

    /// LINK.l
    fn op_link_l(&mut self, instr: &Instruction) -> Result<()> {
        let sp = self.regs.read_a::<Address>(7).wrapping_sub(4);
        let addr = self.regs.read_a::<Address>(instr.get_op2());

        let displacement = {
            let msb = self.fetch_pump()? as Long;
            let lsb = self.fetch_pump()? as Long;
            ((msb << 16) | lsb) as i32
        };

        self.write_ticks(sp, addr)?;
        self.regs.write_a(instr.get_op2(), sp);
        self.regs.read_a_predec::<Address>(7, 4);
        self.regs.write_a(7, sp.wrapping_add_signed(displacement));

        Ok(())
    }

    /// UNLINK
    fn op_unlink(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.regs.read_a::<Address>(instr.get_op2());
        let val = self.read_ticks::<Address>(addr)?;
        self.regs.write_a(7, addr.wrapping_add(4));
        self.regs.write_a(instr.get_op2(), val);
        Ok(())
    }

    /// RESET
    fn op_reset(&mut self, _instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        debug!("RESET instruction");
        self.advance_cycles(128)?;

        // The MacII / System 4.2 restart routine relies on part of a JMP
        // being in the prefetch queue because pulling on reset will re-
        // activate overlay. Re-fill now to have the full JMP pre-fetched.
        self.prefetch_refill()?;

        // Pull on reset
        self.bus.reset(false)?;

        // The (external) FPU and PMMU are connected to the RESET line,
        // so we reset them here.
        // Not for the models with a CPU with a built-in FPU.
        if CPU_TYPE == M68020 {
            self.regs.fpu = FpuRegisterFile::default();
            self.regs.pmmu = PmmuRegisterFile::default();
        }

        Ok(())
    }

    /// RTE
    fn op_rte(&mut self, _instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        if CPU_TYPE == M68000 {
            // 68000 version
            let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
            let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
            *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(6);
            self.set_sr(sr);
            self.set_pc(pc)?;
            self.prefetch_refill()?;

            self.test_step_out();
            return Ok(());
        }

        // 68020+ version
        let ssp = self.regs.ssp();
        let format = (self.read_ticks::<Word>(ssp.wrapping_add(6))? & 0b1111_0000_0000_0000) >> 12;
        match format {
            0b0000 => {
                // Normal format
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(8);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b1010 => {
                // Bus error at instruction boundary
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(32);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b1011 => {
                // Bus error during instruction
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(92);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            _ => bail!("Unknown exception frame format: {:04b}", format),
        }

        self.prefetch_refill()?;
        self.test_step_out();

        Ok(())
    }

    /// RTS
    fn op_rts(&mut self, _instr: &Instruction) -> Result<()> {
        let pc = self.read_ticks(self.regs.read_a(7))?;
        self.regs.read_a_postinc::<Address>(7, 4);
        self.set_pc(pc)?;
        self.prefetch_refill()?;

        self.test_step_out();

        Ok(())
    }

    /// RTR
    fn op_rtr(&mut self, _instr: &Instruction) -> Result<()> {
        let sp = self.regs.read_a::<Address>(7);
        let ccr = self.read_ticks::<Word>(sp.wrapping_add(0))? as Byte;
        let pc = self.read_ticks(sp.wrapping_add(2))?;
        self.regs.read_a_postinc::<Address>(7, 6);
        self.regs.sr.set_ccr(ccr);
        self.set_pc(pc)?;
        self.prefetch_refill()?;
        Ok(())
    }

    /// JMP/JSR
    fn op_jmp_jsr(&mut self, instr: &Instruction) -> Result<()> {
        if instr.needs_extword() && CPU_TYPE == M68000 {
            // Pre-load extension word from prefetch queue
            // to avoid reads in calc_ea_addr().
            instr.fetch_extword(|| self.fetch())?;

            // Advance PC for this last fetch to get the correct value in the PC addressing
            // modes.
            self.regs.pc = self.regs.pc.wrapping_add(2) & ADDRESS_MASK;
        }

        let pc = match instr.get_addr_mode()? {
            AddressingMode::AbsoluteShort => {
                self.advance_cycles(2)?;
                self.regs.pc = self.regs.pc.wrapping_add(2) & ADDRESS_MASK;
                self.fetch()?.expand_sign_extend()
            }
            AddressingMode::AbsoluteLong => {
                let h = self.fetch()? as Address;
                let l = self.fetch()? as Address;
                self.regs.pc = self.regs.pc.wrapping_add(2) & ADDRESS_MASK;
                (h << 16) | l
            }
            AddressingMode::PCDisplacement | AddressingMode::PCIndex => {
                self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?
            }
            _ => self.calc_ea_addr::<Address>(instr, instr.get_addr_mode()?, instr.get_op2())?,
        };

        // Idle cycles
        match instr.get_addr_mode()? {
            AddressingMode::IndirectDisplacement | AddressingMode::PCDisplacement => {
                self.advance_cycles(2)?;
            }
            AddressingMode::IndirectIndex | AddressingMode::PCIndex => self.advance_cycles(4)?,
            _ => (),
        };

        self.step_over_addr = Some(self.get_fetch_addr());

        // Execute the jump
        let old_pc = self.regs.pc;
        self.set_pc(pc)?;
        self.prefetch_pump()?;

        if instr.mnemonic == InstructionMnemonic::JSR {
            // Push return address to the stack
            let sp = self.regs.read_a_predec(7, 4);
            self.write_ticks(sp, old_pc.wrapping_add(2) & ADDRESS_MASK)?;
        }

        self.prefetch_refill()?;
        Ok(())
    }

    /// MOVEM memory to register
    fn op_movem_reg<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let mask = self.fetch_pump()?;
        let mut addr = self.calc_ea_addr_no_mod::<T>(instr, instr.get_op2())?;

        let regs = if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
            Either::Left(MOVEM_REGS.iter().rev())
        } else {
            Either::Right(MOVEM_REGS.iter())
        };

        for (_, &reg) in regs.enumerate().filter(|(i, _)| mask & (1 << i) != 0) {
            if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                addr = addr.wrapping_sub(std::mem::size_of::<T>() as Address);
            }

            let v = self.read_ticks::<T>(addr)?;
            self.regs.write(reg, v.expand_sign_extend());

            if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
                addr = addr.wrapping_add(std::mem::size_of::<T>() as Address);
            }
        }

        // Discarded read
        if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
            addr = addr.wrapping_sub(std::mem::size_of::<T>() as Address);
        }
        self.read_ticks::<Word>(addr)?;

        // Update the EA An register with the final address on predec/postinc
        match instr.get_addr_mode()? {
            AddressingMode::IndirectPostInc | AddressingMode::IndirectPreDec => {
                self.regs.write_a(instr.get_op2(), addr);
            }
            _ => (),
        }

        Ok(())
    }

    /// MOVEM register to memory
    fn op_movem_mem<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let mask = self.fetch_pump()?;
        let mut addr = self.calc_ea_addr_no_mod::<T>(instr, instr.get_op2())?;

        let regs = if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
            Either::Left(MOVEM_REGS.iter().rev())
        } else {
            Either::Right(MOVEM_REGS.iter())
        };

        for (_, &reg) in regs.enumerate().filter(|(i, _)| mask & (1 << i) != 0) {
            let v = self.regs.read::<T>(reg);
            if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                addr = addr.wrapping_sub(std::mem::size_of::<T>() as Address);
                self.write_ticks_wflip(addr, v)?;
            } else {
                self.write_ticks(addr, v)?;
                addr = addr.wrapping_add(std::mem::size_of::<T>() as Address);
            }
        }

        // Update the EA An register with the final address on predec/postinc
        match instr.get_addr_mode()? {
            AddressingMode::IndirectPreDec => self.regs.write_a(instr.get_op2(), addr),
            AddressingMode::IndirectPostInc => self.regs.write_a(instr.get_op2(), addr),
            _ => (),
        }

        Ok(())
    }

    /// CHK
    fn op_chk<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let max = self
            .read_ea::<T>(instr, instr.get_op2())?
            .expand_sign_extend() as i32;
        let value = self.regs.read_d::<T>(instr.get_op1()).expand_sign_extend() as i32;

        let (_result, ccr) =
            Self::alu_sub::<T>(T::chop(max as u32), T::chop(value as u32), self.regs.sr);
        let t = RegisterSR::default().with_ccr(ccr);

        match instr.get_addr_mode()? {
            AddressingMode::Indirect => self.advance_cycles(2)?,
            _ => (),
        }

        self.regs.sr.set_n(value < 0);
        self.regs.sr.set_z(value == 0);
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);

        if t.v() || t.n() {
            // Short trap
            match instr.get_addr_mode()? {
                AddressingMode::Indirect => {
                    self.advance_cycles(6)?;
                }
                _ => self.advance_cycles(8)?,
            }
            // Offset PC correctly for exception stack frame for CHK
            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_CHK, None);
        } else if self.regs.sr.n() {
            // Long trap
            match instr.get_addr_mode()? {
                AddressingMode::Indirect => self.advance_cycles(8)?,
                _ => self.advance_cycles(10)?,
            }
            // Offset PC correctly for exception stack frame for CHK
            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_CHK, None);
        }

        match instr.get_addr_mode()? {
            AddressingMode::Indirect => self.advance_cycles(4)?,
            _ => self.advance_cycles(6)?,
        }
        Ok(())
    }

    /// Condition test for Scc/DBcc/Bcc
    fn cc(&self, condition: usize) -> bool {
        match condition {
            // True
            0b0000 => true,
            // False
            0b0001 => false,
            // Higher
            0b0010 => !self.regs.sr.c() && !self.regs.sr.z(),
            // Lower or same
            0b0011 => self.regs.sr.c() || self.regs.sr.z(),
            // Carry Clear
            0b0100 => !self.regs.sr.c(),
            // Carry Set
            0b0101 => self.regs.sr.c(),
            // Not Equal
            0b0110 => !self.regs.sr.z(),
            // Equal
            0b0111 => self.regs.sr.z(),
            // Overflow Clear
            0b1000 => !self.regs.sr.v(),
            // Overflow Set
            0b1001 => self.regs.sr.v(),
            // Plus
            0b1010 => !self.regs.sr.n(),
            // Minus
            0b1011 => self.regs.sr.n(),
            // Greater or Equal
            0b1100 => self.regs.sr.n() == self.regs.sr.v(),
            // Less Than
            0b1101 => self.regs.sr.n() != self.regs.sr.v(),
            // Greater Than
            0b1110 => self.regs.sr.n() == self.regs.sr.v() && !self.regs.sr.z(),
            // Less or Equal
            0b1111 => self.regs.sr.n() != self.regs.sr.v() || self.regs.sr.z(),

            _ => unreachable!(),
        }
    }

    /// Scc
    fn op_scc(&mut self, instr: &Instruction) -> Result<()> {
        // Discarded read
        self.read_ea::<Byte>(instr, instr.get_op2())?;

        self.prefetch_pump()?;

        let result = if self.cc(instr.get_cc()) {
            if instr.get_addr_mode()? == AddressingMode::DataRegister {
                self.advance_cycles(2)?;
            }
            0xFF
        } else {
            0
        };

        self.write_ea::<Byte>(instr, instr.get_op2(), result)?;
        Ok(())
    }

    /// DBcc
    fn op_dbcc(&mut self, instr: &Instruction) -> Result<()> {
        instr.fetch_extword(|| self.fetch())?;
        let displacement = instr.get_displacement()?;

        self.advance_cycles(2)?; // idle

        if !self.cc(instr.get_cc()) {
            let dn = self.regs.read_d::<Word>(instr.get_op2()).wrapping_sub(1);
            self.regs.write_d::<Word>(instr.get_op2(), dn);

            if dn != 0xFFFF {
                self.history_current.branch_taken = Some(true);

                let pc = self
                    .regs
                    .pc
                    .wrapping_add_signed(displacement)
                    .wrapping_add(2);
                self.set_pc(pc)?;

                // Trigger address error now if unaligned..
                self.prefetch_refill()?;
            } else {
                // Loop terminated
                self.history_current.branch_taken = Some(false);
                self.advance_cycles(4)?; // idle
            }
        } else {
            self.history_current.branch_taken = Some(false);
            self.advance_cycles(2)?; // idle
        }

        Ok(())
    }

    /// Bcc/BSR
    fn op_bcc<const BSR: bool>(&mut self, instr: &Instruction) -> Result<()> {
        let displacement = if instr.get_bxx_displacement() == 0 {
            instr.fetch_extword(|| self.fetch())?;
            instr.get_displacement()?
        } else if CPU_TYPE >= M68020 && instr.get_bxx_displacement_raw() == 0xFF {
            let msb = self.fetch_pump()? as Address;
            let lsb = self.fetch_pump()? as Address;
            // -4 since we just nudged the PC twice
            ((msb << 16) | lsb) as i32 - 4
        } else {
            instr.get_bxx_displacement()
        };

        self.advance_cycles(2)?; // idle

        if BSR || self.cc(instr.get_cc()) {
            // Branch taken
            self.history_current.branch_taken = Some(true);

            if BSR {
                // Push current PC to stack
                let addr = self.regs.read_a_predec(7, std::mem::size_of::<Long>());

                // For .b and .w we add an offset because they fetch the
                // displacement from the prefetch queue and therefore need
                // PC adjustment.
                // For .l, the PC is already adjusted because of the use of
                // fetch_pump().
                let stack_pc = if instr.get_bxx_displacement() == 0 {
                    // Offset by instruction + displacement word
                    self.regs.pc.wrapping_add(4)
                } else {
                    // Offset by instruction
                    self.regs.pc.wrapping_add(2)
                };
                self.write_ticks(addr, stack_pc)?;

                self.step_over_addr = Some(self.get_fetch_addr());
            }
            let pc = self
                .regs
                .pc
                .wrapping_add_signed(displacement)
                .wrapping_add(2);
            self.set_pc(pc)?;

            // Trigger address error now if unaligned..
            self.prefetch_refill()?;
        } else {
            // Branch not taken
            self.history_current.branch_taken = Some(false);

            self.advance_cycles(2)?; // idle
        }
        Ok(())
    }

    /// MOVEQ
    fn op_moveq(&mut self, instr: &Instruction) -> Result<()> {
        let value: Long = (instr.data as u8).expand_sign_extend();

        self.regs.write_d(instr.get_op1(), value);

        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);
        self.regs.sr.set_z(value == 0);
        self.regs.sr.set_n(value & Long::msb() != 0);

        Ok(())
    }

    /// EXG
    fn op_exg(&mut self, instr: &Instruction) -> Result<()> {
        let (reg_l, reg_r) = instr.get_exg_ops()?;

        let left = self.regs.read::<Long>(reg_l);
        let right = self.regs.read::<Long>(reg_r);
        self.regs.write(reg_l, right);
        self.regs.write(reg_r, left);

        self.prefetch_pump()?;
        self.advance_cycles(2)?; // idle

        Ok(())
    }

    /// ASd, LSd, ROd, ROXd
    fn op_shrot<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, usize, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let count = match instr.get_sh_count() {
            Either::Left(i) => i as usize,
            Either::Right(r) => (self.regs.read::<Long>(r) % 64) as usize,
        };

        self.prefetch_pump()?;

        let value = self.regs.read_d::<T>(instr.get_op2());
        let (result, ccr) = calcfn(value, count, self.regs.sr);
        self.regs.write_d(instr.get_op2(), result);
        self.regs.sr.set_ccr(ccr);

        self.advance_cycles(2 * count)?;

        match std::mem::size_of::<T>() {
            4 => self.advance_cycles(4)?,
            _ => self.advance_cycles(2)?,
        };

        Ok(())
    }

    /// ASd, LSd, ROd, ROXd (effective address, always Word)
    fn op_shrot_ea(
        &mut self,
        instr: &Instruction,
        calcfn: fn(Word, usize, RegisterSR) -> (Word, u8),
    ) -> Result<()> {
        let value = self.read_ea::<Word>(instr, instr.get_op2())?;

        self.prefetch_pump()?;

        let (result, ccr) = calcfn(value, 1, self.regs.sr);
        self.write_ea::<Word>(instr, instr.get_op2(), result)?;
        self.regs.sr.set_ccr(ccr);

        Ok(())
    }

    /// MOVEC
    fn op_movec(&mut self, instr: &Instruction) -> Result<()> {
        // Bus access and cycles are an approximation based on UM/PRM
        // This has to be a fetch from the prefetch queue for PC to stay correct
        // in the exception frame.
        instr.fetch_extword(|| self.fetch())?;

        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        if instr.movec_ctrl_to_gen() {
            let val = self.regs.read::<Long>(instr.movec_ctrlreg()?.into());
            self.regs.write(instr.movec_reg()?, val);
            self.advance_cycles(4)?;
        } else {
            let val = self.regs.read::<Long>(instr.movec_reg()?);
            let destreg: Register = instr.movec_ctrlreg()?.into();

            if destreg == Register::CACR {
                // I-cache operations
                let val = RegisterCACR(val);
                if val.c() {
                    // Full clear
                    self.icache_tags.fill(ICACHE_TAG_INVALID);
                }
                if val.ce() {
                    // Clear specified index
                    let index = ((self.regs.caar & ICACHE_INDEX_MASK) >> 2) as usize;
                    self.icache_tags[index] = ICACHE_TAG_INVALID;
                }

                self.regs.write(destreg, val.with_c(false).with_ce(false).0);
            } else {
                self.regs.write(destreg, val);
            }
            self.advance_cycles(2)?;
        }

        Ok(())
    }

    /// RTD
    fn op_rtd(&mut self, _instr: &Instruction) -> Result<()> {
        // Bus access and cycles are an approximation based on UM/PRM
        let displacement = self.fetch()?.expand_sign_extend() as i32;
        let pc = self.read_ticks(self.regs.read_a(7))?;
        let sp = self.regs.read_a::<Address>(7);
        self.regs
            .write_a(7, sp.wrapping_add_signed(4 + displacement));
        self.set_pc(pc)?;
        self.prefetch_refill()?;

        self.test_step_out();

        Ok(())
    }

    /// BFCLR
    fn op_bfclr(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                offset &= 31;
                width = ((width.wrapping_sub(1)) & 31) + 1;

                let data = self.regs.read_d::<Long>(instr.get_op2());

                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask = mask_base.rotate_right(offset);

                self.regs.sr.set_n((data << offset) & 0x80000000 != 0);
                self.regs.sr.set_z(data & mask == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);
                self.regs.write_d(instr.get_op2(), data & !mask);
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address with byte offset
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                // Create mask for the main 32-bit word
                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask_long = mask_base >> (offset as isize);

                let data_long = self.read_ticks::<Long>(ea)?;
                self.regs
                    .sr
                    .set_n((data_long << (offset as isize)) & 0x80000000 != 0);
                self.regs.sr.set_z(data_long & mask_long == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                self.write_ticks(ea, data_long & !mask_long)?;

                // Handle bit fields that cross 32-bit boundaries
                if (width as i32 + offset) > 32 {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Update Z flag with the extended part
                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }

                    self.write_ticks(ea.wrapping_add(4), data_byte & !mask_byte)?;
                }
            }
        }

        Ok(())
    }

    /// BFEXTU
    fn op_bfextu(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Ensure offset is in range 0-31
                offset &= 31;

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mut data = self.regs.read_d::<Long>(instr.get_op2());

                data = data.rotate_left(offset);

                // Set N flag from the rotated data
                self.regs.sr.set_n(data & 0x80000000 != 0);

                // Extract the bits
                data >>= 32 - width;

                self.regs.sr.set_z(data == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                self.regs.write_d(sec.reg(), data);
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;

                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mut data = self.read_ticks::<Long>(ea)?;
                data <<= offset as isize;

                // If the bit field crosses a 32-bit boundary, read an additional byte
                if (offset + width as i32) > 32 {
                    let extra_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))? as Long;
                    data |= (extra_byte << (offset as isize)) >> 8;
                }

                // Set N flag from the data before shifting for extraction
                self.regs.sr.set_n(data & 0x80000000 != 0);

                // Right shift to extract the bits
                data >>= 32 - width;

                self.regs.sr.set_z(data == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);
                self.regs.write_d(sec.reg(), data);
            }
        }

        Ok(())
    }

    /// BFEXTS
    fn op_bfexts(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                offset &= 31;
                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mut data = self.regs.read_d::<Long>(instr.get_op2());
                data = data.rotate_left(offset);

                // Set N flag from the rotated data
                self.regs.sr.set_n(data & 0x80000000 != 0);

                let result = data.signed_shr(32 - width);
                self.regs.sr.set_z(result == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);
                self.regs.write_d(sec.reg(), result);
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mut data = self.read_ticks::<Long>(ea)?;
                data <<= offset as isize;

                // If the bit field crosses a 32-bit boundary, read an additional byte
                if (offset + width as i32) > 32 {
                    let extra_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))? as Long;
                    data |= (extra_byte << (offset as isize)) >> 8;
                }

                // Set N flag from the data before shifting for extraction
                self.regs.sr.set_n(data & 0x80000000 != 0);

                let result = data.signed_shr(32 - width);
                self.regs.sr.set_z(result == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);
                self.regs.write_d(sec.reg(), result);
            }
        }

        Ok(())
    }

    /// BFFFO
    fn op_bfffo(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                offset &= 31;
                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mut data = self.regs.read_d::<Long>(instr.get_op2());
                data = data.rotate_left(offset);

                // Set N flag from the rotated data
                self.regs.sr.set_n(data & 0x80000000 != 0);

                // Right shift to extract the bits
                data >>= 32 - width;

                self.regs.sr.set_z(data == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // Find first one bit from MSB to LSB
                let mut result_offset = offset;
                for bit in (0..width).rev() {
                    if data & (1 << bit) != 0 {
                        break;
                    }
                    result_offset += 1;
                }

                self.regs.write_d(sec.reg(), result_offset);
            }
            _ => {
                // Memory version
                let offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address with byte offset
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset / 8);
                let mut local_offset = offset % 8;

                // Handle negative offsets
                if local_offset < 0 {
                    local_offset += 8;
                    ea = ea.wrapping_sub(1);
                }

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mut data = self.read_ticks::<Long>(ea)?;
                data <<= local_offset as isize;

                // If the bit field crosses a 32-bit boundary, read an additional byte
                if (local_offset + width as i32) > 32 {
                    let extra_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))? as Long;
                    data |= (extra_byte << (local_offset as isize)) >> 8;
                }

                // Set N flag from the data before shifting for extraction
                self.regs.sr.set_n(data & 0x80000000 != 0);

                // Right shift to extract the bits
                data >>= 32 - width;

                self.regs.sr.set_z(data == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // Find first one bit from MSB to LSB
                let mut result_offset = offset;
                for bit in (0..width).rev() {
                    if data & (1 << bit) != 0 {
                        break;
                    }
                    result_offset += 1;
                }

                self.regs.write_d(sec.reg(), result_offset as Long);
            }
        }

        Ok(())
    }

    /// BFSET
    fn op_bfset(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                offset &= 31;
                width = ((width.wrapping_sub(1)) & 31) + 1;

                let data = self.regs.read_d::<Long>(instr.get_op2());

                // Create mask: 0xffffffff << (32 - width), then rotate right by offset
                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask = mask_base.rotate_right(offset);

                self.regs.sr.set_n((data << offset) & 0x80000000 != 0);
                self.regs.sr.set_z(data & mask == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);
                self.regs.write_d(instr.get_op2(), data | mask);
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address with byte offset
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                // Create mask for the main 32-bit word
                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask_long = mask_base >> (offset as isize);

                let data_long = self.read_ticks::<Long>(ea)?;

                self.regs
                    .sr
                    .set_n((data_long << (offset as isize)) & 0x80000000 != 0);
                self.regs.sr.set_z(data_long & mask_long == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                self.write_ticks(ea, data_long | mask_long)?;

                // Handle bit fields that cross 32-bit boundaries
                if (width as i32 + offset) > 32 {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Update Z flag with the extended part
                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }

                    self.write_ticks(ea.wrapping_add(4), data_byte | mask_byte)?;
                }
            }
        }

        Ok(())
    }

    /// BFTST
    fn op_bftst(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                offset &= 31;
                width = ((width.wrapping_sub(1)) & 31) + 1;

                let data = self.regs.read_d::<Long>(instr.get_op2());

                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask = mask_base.rotate_right(offset);

                self.regs.sr.set_n((data << offset) & 0x80000000 != 0);
                self.regs.sr.set_z(data & mask == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // No data modification for BFTST
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address with byte offset
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask_long = mask_base >> (offset as isize);

                let data_long = self.read_ticks::<Long>(ea)?;

                let n_bit = (data_long & (0x80000000 >> (offset as isize))) != 0;
                self.regs.sr.set_n(n_bit);
                self.regs.sr.set_z(data_long & mask_long == 0);

                // Handle bit fields that cross 32-bit boundaries
                if (width as i32 + offset) > 32 {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }
                }

                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // No data modification for BFTST
            }
        }

        Ok(())
    }

    /// BFCHG
    fn op_bfchg(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let data_reg = instr.get_op2();
                let data = self.regs.read_d::<Long>(data_reg);

                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask = mask_base.rotate_right(offset);

                // Set flags
                self.regs.sr.set_n((data << offset) & 0x80000000 != 0);
                self.regs.sr.set_z(data & mask == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                self.regs.write_d(data_reg, data ^ mask);
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address with byte offset
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask_long = mask_base >> (offset as isize);

                let data_long = self.read_ticks::<Long>(ea)?;
                self.regs
                    .sr
                    .set_n((data_long << (offset as isize)) & 0x80000000 != 0);
                self.regs.sr.set_z(data_long & mask_long == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // Write result back
                self.write_ticks(ea, data_long ^ mask_long)?;

                // Handle bit fields that cross 32-bit boundaries
                if (width as i32 + offset) > 32 {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Update Z flag with the extended part
                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }

                    // Write the extended part
                    self.write_ticks(ea.wrapping_add(4), data_byte ^ mask_byte)?;
                }
            }
        }

        Ok(())
    }

    /// BFINS
    fn op_bfins(&mut self, instr: &Instruction) -> Result<()> {
        let sec = BfxExtWord(self.fetch_pump()?);

        match instr.get_addr_mode()? {
            AddressingMode::DataRegister => {
                // Data register version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) & 31
                } else {
                    sec.offset()
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Ensure offset is in range 0-31
                offset &= 31;

                width = ((width.wrapping_sub(1)) & 31) + 1;

                let data_reg = instr.get_op2();
                let mut data = self.regs.read_d::<Long>(data_reg);

                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask = mask_base.rotate_right(offset);

                // Get insert data from source register, masked to width
                let mut insert = self.regs.read_d::<Long>(sec.reg());
                insert <<= 32 - width;

                // Set flags on the insert data before rotating
                self.regs.sr.set_n(insert & 0x80000000 != 0);
                self.regs.sr.set_z(insert == 0);

                // Rotate insert data to align with destination
                insert = insert.rotate_right(offset);

                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                data &= !mask;
                data |= insert;
                self.regs.write_d(data_reg, data);
            }
            _ => {
                // Memory version
                let mut offset = if sec.fdo() {
                    self.regs.read_d::<Long>(sec.offset_reg()) as i32
                } else {
                    sec.offset() as i32
                };

                let mut width = if sec.fdw() {
                    self.regs.read_d::<Long>(sec.width_reg())
                } else {
                    sec.width()
                };

                // Calculate effective address with byte offset
                let mut ea =
                    self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;
                ea = ea.wrapping_add_signed(offset.div_euclid(8));
                offset = offset.rem_euclid(8);

                width = ((width.wrapping_sub(1)) & 31) + 1;

                // Create mask for the bits to be inserted
                let mask_base = 0xFFFFFFFF_u32 << (32 - width);
                let mask_long = mask_base >> (offset as isize);

                let mut insert_base = self.regs.read_d::<Long>(sec.reg());
                insert_base <<= 32 - width;

                // Set flags based on insert data
                self.regs.sr.set_n(insert_base & 0x80000000 != 0);
                self.regs.sr.set_z(insert_base == 0);

                let insert_long = insert_base >> (offset as isize);

                let data_long = self.read_ticks::<Long>(ea)?;

                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // Combine data and insert values and write back
                self.write_ticks(ea, (data_long & !mask_long) | insert_long)?;

                // Handle bit fields that cross 32-bit boundaries
                if (width as i32 + offset) > 32 {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let insert_byte = (insert_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Not updating Z flag here, it is based on the inserted data
                    // and was already set above.

                    // Write the extended part
                    self.write_ticks(ea.wrapping_add(4), (data_byte & !mask_byte) | insert_byte)?;
                }
            }
        }

        Ok(())
    }

    /// MULx (Long)
    fn op_mulx_l(&mut self, instr: &Instruction) -> Result<()> {
        let extword = MulxExtWord(self.fetch_pump()?);

        let result = if extword.signed() {
            let a = self.regs.read_d::<Long>(extword.dl()) as i32 as i64;
            let b = self.read_ea::<Long>(instr, instr.get_op2())? as i32 as i64;

            // Computation time
            self.advance_cycles(34 + (((b << 1) ^ b).count_ones() as Ticks) * 2)?;

            a.wrapping_mul(b)
        } else {
            let a = self.regs.read_d::<Long>(extword.dl()) as u64;
            let b = self.read_ea::<Long>(instr, instr.get_op2())? as u64;

            // Computation time
            self.advance_cycles(34 + (((b << 1) ^ b).count_ones() as Ticks) * 2)?;

            a.wrapping_mul(b) as i64
        };

        self.prefetch_pump()?;

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        if extword.size() {
            self.regs.sr.set_n(result & (1 << 63) != 0);
        } else {
            self.regs.sr.set_n(result & (1 << 31) != 0);
        }
        self.regs.sr.set_z(result == 0);

        self.regs.write_d(extword.dl(), result as i32 as Long);
        if extword.size() {
            self.regs
                .write_d(extword.dh(), (result >> 32) as i32 as Long);
        }

        Ok(())
    }

    /// DIVU/DIVS (Long)
    fn op_divx_l(&mut self, instr: &Instruction) -> Result<()> {
        let extword = DivlExtWord(self.fetch_pump()?);
        let dr = self.regs.read_d::<Long>(extword.dr());
        let dq = self.regs.read_d::<Long>(extword.dq());

        let dividend = if extword.size() {
            // 64-bit
            (u64::from(dr) << 32) | u64::from(dq)
        } else {
            u64::from(dq)
        };
        let divisor = self.read_ea::<Long>(instr, instr.get_op2())? as u64;

        if divisor == 0 {
            // Division by zero
            self.advance_cycles(4)?;
            self.regs.sr.set_n(false);
            self.regs.sr.set_c(false);
            self.regs.sr.set_z(false);
            self.regs.sr.set_v(false);

            return self.raise_exception(ExceptionGroup::Group2, VECTOR_DIV_ZERO, None);
        }

        self.prefetch_pump()?;

        let (quotient, remainder) = match (extword.signed(), extword.size()) {
            (false, false) => {
                // 32-bit unsigned
                (dividend / divisor, dividend % divisor)
            }
            (true, false) => {
                // 32-bit signed
                (
                    ((dividend as u32 as i32) / (divisor as u32 as i32)) as i64 as u64,
                    ((dividend as u32 as i32) % (divisor as u32 as i32)) as i64 as u64,
                )
            }
            (false, true) => {
                // 64-bit unsigned
                (dividend / divisor, dividend % divisor)
            }
            (true, true) => {
                // 64-bit signed
                (
                    ((dividend as i64) / (divisor as i64)) as u64,
                    ((dividend as i64) % (divisor as i64)) as u64,
                )
            }
        };

        // Check overflow conditions on 64-bit divisions if the result exceeds 32-bit
        if extword.size() && ![u32::MIN, u32::MAX].contains(&((quotient >> 32) as u32)) {
            debug!("DIV.l overflow");
            self.regs.sr.set_v(true);
            return Ok(());
        }

        // 76-79 cycles
        self.advance_cycles(76)?;

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(quotient & (1 << 31) != 0);
        self.regs.sr.set_z(quotient == 0);

        self.regs.write_d(extword.dr(), remainder as u32);
        self.regs.write_d(extword.dq(), quotient as u32);

        Ok(())
    }

    /// CAS
    fn op_cas<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        if instr.data & 0b111111 == 0b111100 {
            // Actually CAS2
            return self.op_cas2::<T>(instr);
        }

        let extword = self.fetch()?;
        let dc = (extword & 0b111) as usize;
        let du = ((extword >> 6) & 0b111) as usize;

        let ea_op = self.read_ea::<T>(instr, instr.get_op2())?;
        let comp_op = self.regs.read_d::<T>(dc);
        let update_op = self.regs.read_d::<T>(du);
        let (_, ccr) = Self::alu_sub(ea_op, comp_op, self.regs.sr);

        let old_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        self.regs.sr.set_x(old_x); // X is unaffected

        if self.regs.sr.z() {
            self.write_ea(instr, instr.get_op2(), update_op)?;
        } else {
            self.regs.write_d(dc, ea_op);
        }

        Ok(())
    }

    /// CAS2
    fn op_cas2<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        debug_assert_eq!(instr.data & 0b111111, 0b111100);
        if std::mem::size_of::<T>() == 1 {
            // CAS2 not valid as byte size
            return self.raise_illegal_instruction();
        }

        let extword1 = self.fetch()?;
        let extword2 = self.fetch()?;

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
        let mem_addr1 = self.regs.read::<Address>(rn1);
        let mem_addr2 = self.regs.read::<Address>(rn2);
        let mem_op1 = self.read_ticks::<T>(mem_addr1)?;
        let mem_op2 = self.read_ticks::<T>(mem_addr2)?;
        let comp_op1 = self.regs.read_d::<T>(dc1);
        let comp_op2 = self.regs.read_d::<T>(dc2);
        let update_op1 = self.regs.read_d::<T>(du1);
        let update_op2 = self.regs.read_d::<T>(du2);

        let old_x = self.regs.sr.x();

        // First round
        let (_, ccr) = Self::alu_sub(mem_op1, comp_op1, self.regs.sr);
        self.regs.sr.set_ccr(ccr);
        self.regs.sr.set_x(old_x); // X is unaffected
        if self.regs.sr.z() {
            // Second round
            let (_, ccr) = Self::alu_sub(mem_op2, comp_op2, self.regs.sr);
            self.regs.sr.set_ccr(ccr);
            self.regs.sr.set_x(old_x); // X is unaffected
            if self.regs.sr.z() {
                // Success
                // Do these need to happen atomically?..
                self.write_ticks(mem_addr1, update_op1)?;
                self.write_ticks(mem_addr2, update_op2)?;

                return Ok(());
            }
        }

        // Failed
        // Write Dc1 last, as the PRM states that MemOp1 will be written to
        // the register if Dc1 == Dc2
        self.regs.write_d(dc2, mem_op2);
        self.regs.write_d(dc1, mem_op1);

        Ok(())
    }
}

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool> Tickable
    for CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    fn tick(&mut self, _ticks: Ticks) -> Result<Ticks> {
        self.step()?;

        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::testbus::Testbus;
    use crate::cpu_m68k::{CpuM68000, CpuM68020, M68000_ADDRESS_MASK, M68020_ADDRESS_MASK};

    use super::*;

    #[test]
    fn sr_68000() {
        let mut cpu = CpuM68000::<Testbus<Address, Byte>>::new(Testbus::new(M68000_ADDRESS_MASK));
        cpu.set_sr(0xFFFF);
        assert!(!cpu.regs.sr.m());
    }

    #[test]
    fn sr_68020() {
        let mut cpu = CpuM68020::<Testbus<Address, Byte>>::new(Testbus::new(M68020_ADDRESS_MASK));
        cpu.set_sr(0xFFFF);
        assert!(cpu.regs.sr.m());
    }
}
