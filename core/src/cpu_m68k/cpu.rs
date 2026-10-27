use crate::cpu_m68k::FpuM68kType;
use crate::cpu_m68k::bus::AccessKind;
use crate::cpu_m68k::bus_transaction::bus_transaction;
use crate::cpu_m68k::cache::{CacheScope, CacheSelect, CpuCaches, FETCH_HIT_TICKS};
use std::collections::VecDeque;

use anyhow::{Result, anyhow, bail};
use arrayvec::ArrayVec;
use either::Either;
use log::*;
use num_traits::{FromBytes, PrimInt, ToBytes};
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bus::{Address, Bus, InspectableBus, IrqSource};
use crate::cpu_m68k::fpu::regs::FpuRegisterFile;
use crate::cpu_m68k::pmmu::regs::PmmuRegisterFile;
use crate::cpu_m68k::pmmu::translate::atc_generation_default;
use crate::cpu_m68k::{
    M68000_SR_MASK, M68020_CACR_MASK, M68030, M68030_CACR_MASK, M68040, M68040_CACR_MASK,
    M68040_SR_MASK,
};
use crate::tickable::{Tickable, Ticks};
use crate::types::{Byte, LatchingEvent, Long, Word};

use super::instruction::{
    AddressingMode, BfxExtWord, Direction, DivlExtWord, Instruction, InstructionMnemonic,
    MovecCtrlReg, MulxExtWord,
};
use super::regs::{Register, RegisterFile, RegisterSR, RestartRegisterFile};
use super::timing040::{self, Exec040, FullEa040};
use super::{
    CpuM68kType, CpuSized, FPU_NONE, M68000, M68010, M68020, M68020_SR_MASK, M68030_SR_MASK,
    TORDER_HIGHLOW, TORDER_LOWHIGH,
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
    /// M68040 only (MMU page fault)
    pub(in crate::cpu_m68k) atc_fault: bool,
}

/// M68040: a faulted data write waiting in a write-back slot for the access
/// error handler to complete it (M68040UM 8.4.6)
#[derive(Debug, Clone, Copy)]
pub(in crate::cpu_m68k) struct WriteBack040 {
    pub(in crate::cpu_m68k) details: Group0Details,
    pub(in crate::cpu_m68k) data: Long,
    /// The fault is in the second page of a misaligned write
    pub(in crate::cpu_m68k) ma: bool,
    /// A trace exception is pending for the instruction
    pub(in crate::cpu_m68k) trace: bool,
}

/// An entry in the instruction prefetch queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefetchWord {
    /// A successfully fetched instruction word.
    Word(u16),
    /// A prefetch read that bus-faulted; carries the faulting address. The bus
    /// error is raised only when this entry is consumed; failed speculative
    /// fetches are discarded upon branches.
    Fault(Address),
}

impl PrefetchWord {
    pub fn word(self) -> u16 {
        match self {
            Self::Word(w) => w,
            Self::Fault(_) => 0,
        }
    }
}

/// The instruction prefetch queue: two words at most. fetch() pops a word
/// before it pumps the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchQueue {
    words: [PrefetchWord; 2],
    len: usize,
}

impl Default for PrefetchQueue {
    fn default() -> Self {
        Self {
            words: [PrefetchWord::Word(0); 2],
            len: 0,
        }
    }
}

impl PrefetchQueue {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn front(&self) -> Option<&PrefetchWord> {
        self.get(0)
    }

    pub fn get(&self, i: usize) -> Option<&PrefetchWord> {
        self.words[..self.len].get(i)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PrefetchWord> {
        self.words[..self.len].iter()
    }

    pub fn push_back(&mut self, word: PrefetchWord) {
        assert!(self.len < 2, "prefetch queue overflow");
        self.words[self.len] = word;
        self.len += 1;
    }

    pub fn pop_front(&mut self) -> Option<PrefetchWord> {
        if self.len == 0 {
            return None;
        }
        let word = self.words[0];
        self.words[0] = self.words[1];
        self.len -= 1;
        Some(word)
    }
}

impl FromIterator<PrefetchWord> for PrefetchQueue {
    fn from_iter<I: IntoIterator<Item = PrefetchWord>>(iter: I) -> Self {
        let mut queue = Self::default();
        for word in iter {
            queue.push_back(word);
        }
        queue
    }
}

fn prefetch_defer_fault(e: anyhow::Error, fetch_addr: Address) -> Result<PrefetchWord> {
    match e.downcast_ref::<CpuError>() {
        Some(CpuError::BusError(_)) => Ok(PrefetchWord::Fault(fetch_addr)),
        _ => Err(e),
    }
}

/// Reason a PMMU translation failed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cpu_m68k) enum PagefaultCause {
    /// Descriptor DT was Invalid along the walk
    Invalid,
    /// Write access to a page whose descriptor (or any ancestor table) has WP set
    WriteProtected,
    /// Index into a table violated the limit set by the parent long-format
    /// descriptor (RP or long table descriptor): LU=0 = upper limit, LU=1 = lower.
    LimitViolation,
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
    Pagefault(PagefaultCause),
    /// Raise illegal instruction exception
    #[error("Illegal instruction exception")]
    IllegalInstruction,
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

bitfield! {
    /// M68040 access error stack frame Special Status Word
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct Ssw040(pub Word): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Transfer modifier (function code)
        pub tm: u8 @ 0..=2,
        /// Transfer type (0 = normal)
        pub tt: u8 @ 3..=4,
        /// Transfer size (0 = long, 1 = byte, 2 = word, 3 = line)
        pub size: u8 @ 5..=6,
        /// Read (1) / write (0)
        pub rw: bool @ 8,
        /// Locked transfer
        pub lk: bool @ 9,
        /// Fault from the ATC rather than from the bus
        pub atc: bool @ 10,
        /// Misaligned access
        pub ma: bool @ 11,
        /// Continuation: MOVEM
        pub cm: bool @ 12,
        /// Continuation: trace
        pub ct: bool @ 13,
        /// Continuation: unimplemented floating point
        pub cu: bool @ 14,
        /// Continuation: floating point post-instruction
        pub cp: bool @ 15,
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
/// Format error exception vector
pub const VECTOR_FORMAT_ERROR: Address = 0x000038;

/// Function code of the CPU space
const FC_CPU_SPACE: u8 = 7;
/// Coprocessor interface registers of the FPU in the CPU space
const FPU_CIR_BASE: Address = 0x0002_2000;
const FPU_CIR_MASK: Address = 0x1F;
/// Response (null primitive) and control registers of an idle FPU
const FPU_CIR_RESPONSE_NULL: Long = 0x0802_0000;
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
#[derive(Serialize, Deserialize)]
pub struct CpuM68k<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
    TCaches,
> where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    /// Exception occured this step
    pub step_exception: bool,

    /// Trace exception must be inhibited after this instruction
    step_inhibit_trace: bool,

    /// Address of the instruction of this step
    #[serde(skip)]
    step_start_pc: Address,

    /// The F-line exception raised this step is the MC68040 unimplemented
    /// floating-point instruction exception
    #[serde(skip)]
    pub(in crate::cpu_m68k) step_fpu_unimp: bool,

    /// External address/data bus
    pub bus: TBus,

    /// Register state
    pub regs: RegisterFile,

    /// Total cycle counter
    pub cycles: Ticks,

    /// Clocks of the last write that the following internal clocks can
    /// overlap (68020+): the sequencer goes on while the bus controller
    /// finishes the write (MC68020UM 8.1.4, MC68030UM 11.1)
    pub(in crate::cpu_m68k) write_credit: Ticks,

    /// Time at which the FPU finishes the arithmetic operation it runs on its own
    pub(in crate::cpu_m68k) fpu_free_at: Ticks,

    /// FPU data registers that the operation the FPU runs on its own writes, a bit
    /// for each
    pub(in crate::cpu_m68k) fpu_busy_regs: u8,

    /// Execute stage time of the instruction of this step on the 68040
    #[serde(skip)]
    pub(in crate::cpu_m68k) exec040: Exec040,

    /// Base execute stage time of the previous instruction on the 68040
    #[serde(skip)]
    pub(in crate::cpu_m68k) exec040_prev: u16,

    /// Addressing modes of the effective addresses with a full format
    /// extension word of this step, in order, for the 68040 times
    #[serde(skip)]
    pub(in crate::cpu_m68k) step_ea040: [Option<FullEa040>; 2],

    /// Time of last bus sync in cycles
    pub last_bus_sync: Ticks,

    /// Current prefetch queue
    pub prefetch: PrefetchQueue,

    /// 68020/030 cache holding register: the second word of the last
    /// aligned long word fetched
    #[serde(skip)]
    holding: Word,

    /// `holding` is valid, for the next fetch
    #[serde(skip)]
    holding_valid: bool,

    /// Address of the word in `holding`, to check the next fetch is for it
    #[cfg(debug_assertions)]
    #[serde(skip)]
    holding_addr: Address,

    /// Effective Address cache for this step
    pub(in crate::cpu_m68k) step_ea_addr: Option<Address>,

    /// Value to load to an address register by ea_commit().
    pub(in crate::cpu_m68k) step_ea_load: Option<(usize, Address)>,

    /// Instruction decode cache
    #[serde(skip, default = "empty_decode_cache")]
    decode_cache: DecodeCache,

    /// Mask trace exceptions (for tests)
    pub trace_mask: bool,

    /// Active breakpoints
    pub(in crate::cpu_m68k) breakpoints: Vec<Breakpoint>,

    /// Breakpoint hit latch
    #[serde(skip)]
    pub(in crate::cpu_m68k) breakpoint_hit: LatchingEvent,

    /// Next address to jump to for step over
    step_over_addr: Option<Address>,

    /// Instruction history
    #[serde(skip)]
    pub(in crate::cpu_m68k) history: VecDeque<HistoryEntry>,

    /// Current instruction history item
    #[serde(skip)]
    pub(in crate::cpu_m68k) history_current: HistoryEntryInstruction,

    /// Keep instruction history?
    #[serde(skip)]
    pub(in crate::cpu_m68k) history_enabled: bool,

    /// System trap history
    #[serde(skip)]
    systrap_history: VecDeque<SystrapHistoryEntry>,

    /// Keep system trap history?
    #[serde(skip)]
    systrap_history_enabled: bool,

    /// PMMU address translation caches
    /// The index is either PMMU_ATC_URP or PMMU_ATC_SRP, depending on which root
    /// pointer is used in the translation.
    ///
    /// The caching method used is a one-dimensional lookup table where the
    /// index is the page, allowing for O(1) lookup. The cache is expanded on
    /// the fly (never shrunk).
    #[serde(with = "crate::cpu_m68k::pmmu::translate::atc_serde")]
    pub(in crate::cpu_m68k) pmmu_atc: [Vec<Option<crate::cpu_m68k::pmmu::translate::PmmuAtcEntry>>;
        crate::cpu_m68k::pmmu::translate::PMMU_ATCS],

    /// Current ATC generation. Entries tagged with an older generation have been flushed
    /// On flush, this is incremented to efficiently invalidate the entire ATC.
    #[serde(default = "crate::cpu_m68k::pmmu::translate::atc_generation_default")]
    pub(in crate::cpu_m68k) pmmu_atc_generation: u32,

    /// Last translations, for data reads, instruction fetches and data
    /// writes
    #[serde(
        skip,
        default = "crate::cpu_m68k::pmmu::translate::last_translations_none"
    )]
    pub(in crate::cpu_m68k) last_translation: [[crate::cpu_m68k::pmmu::translate::LastTranslation;
        crate::cpu_m68k::pmmu::translate::LAST_TRANSLATIONS];
        3],

    /// On-chip caches
    #[serde(default)]
    pub(in crate::cpu_m68k) caches: TCaches,

    /// Register state at the beginning of an instruction to allow
    /// restarting an instruction that caused a mid-instruction
    /// bus fault.
    pub(in crate::cpu_m68k) restart_regs: Option<RestartRegisterFile>,

    /// M68040: data writes of the current instruction may fault into a
    /// write-back slot instead of aborting the instruction
    #[serde(skip)]
    pub(in crate::cpu_m68k) wb040_defer: bool,

    /// M68040: faulted write of the current instruction
    #[serde(skip)]
    pub(in crate::cpu_m68k) wb040: Option<WriteBack040>,

    /// NMI edge trigger level
    in_nmi: bool,
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
    TCaches,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU, TCaches>
where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    /// Instruction history size
    pub const HISTORY_SIZE: usize = 10000;

    pub fn new(bus: TBus) -> Self {
        assert!([M68000, M68020, M68030, M68040].contains(&CPU_TYPE));

        Self {
            bus,
            regs: RegisterFile::new(),
            cycles: 0,
            write_credit: 0,
            fpu_free_at: 0,
            fpu_busy_regs: 0,
            exec040: Exec040::DEFAULT,
            exec040_prev: 0,
            step_ea040: [None; 2],
            last_bus_sync: 0,
            prefetch: PrefetchQueue::default(),
            holding: 0,
            holding_valid: false,
            #[cfg(debug_assertions)]
            holding_addr: 0,
            step_ea_addr: None,
            step_exception: false,
            step_inhibit_trace: false,
            step_start_pc: 0,
            step_fpu_unimp: false,
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
            pmmu_atc_generation: atc_generation_default(),
            last_translation: crate::cpu_m68k::pmmu::translate::last_translations_none(),
            caches: TCaches::default(),
            restart_regs: None,
            wb040_defer: false,
            wb040: None,
            in_nmi: false,
        }
    }

    pub const fn get_type(&self) -> CpuM68kType {
        CPU_TYPE
    }

    /// Returns true if this CPU has a (paged) memory management unit.
    pub const fn has_pmmu(&self) -> bool {
        PMMU
    }

    /// Resets the CPU, loads reset vector and initial SP
    pub fn reset(&mut self) -> Result<()> {
        self.regs = RegisterFile::new();
        self.caches.reset();
        self.pmmu_translation_changed();

        self.cycles = 0;
        self.last_bus_sync = 0;
        self.write_credit = 0;
        self.fpu_free_at = 0;
        self.fpu_busy_regs = 0;
        self.exec040_prev = 0;
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

    /// Makes the caches and memory agree before the host reads, or if
    /// `write` writes, `len` bytes of memory at physical address `paddr`
    pub fn host_flush(&mut self, paddr: Address, len: Address, write: bool)
    where
        TBus: InspectableBus<Address, Byte>,
    {
        self.caches.host_flush(&mut self.bus, paddr, len, write);
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

        // The 68020 and up raise an address error on a prefetch at an odd
        // address (MC68020UM 5.1.2)
        if CPU_TYPE >= M68020 && fetch_addr & 1 != 0 {
            bail!(CpuError::AddressError(Group0Details {
                function_code: self.fc_program(),
                ir: 0,
                size: std::mem::size_of::<Word>(),
                atc_fault: false,
                instruction: true,
                read: true,
                address: fetch_addr,
                // Filled in later
                start_pc: 0,
            }));
        }

        // The 68020/030 always fetch the whole long word, even for an odd
        // word (MC68020UM 5.2.3, MC68030UM 11.2.5.3). After an aligned word,
        // they keep the second one in the cache holding register for the next
        // fetch, with the I-cache enabled or not (MC68020UM 8.1.1, MC68030UM
        // 11.2.2). set_pc() invalidates it.
        let holding = CPU_TYPE == M68020 || CPU_TYPE == M68030;
        let new_item = if holding && self.holding_valid && fetch_addr & 3 == 2 {
            #[cfg(debug_assertions)]
            assert_eq!(self.holding_addr, fetch_addr);
            self.holding_valid = false;
            self.advance_cycles(FETCH_HIT_TICKS)?;
            PrefetchWord::Word(self.holding)
        } else if holding {
            match self.read_ticks_program::<Long>(fetch_addr & !3) {
                Ok(l) if fetch_addr & 3 == 0 => {
                    self.holding = l as Word;
                    self.holding_valid = true;
                    #[cfg(debug_assertions)]
                    {
                        self.holding_addr = fetch_addr.wrapping_add(2) & ADDRESS_MASK;
                    }
                    PrefetchWord::Word((l >> 16) as Word)
                }
                Ok(l) => PrefetchWord::Word(l as Word),
                Err(e) => prefetch_defer_fault(e, fetch_addr)?,
            }
        } else {
            // An instruction cache, if any, is behind the fetch
            match self.read_ticks_program::<Word>(fetch_addr) {
                Ok(w) => PrefetchWord::Word(w),
                Err(e) => prefetch_defer_fault(e, fetch_addr)?,
            }
        };
        self.prefetch.push_back(new_item);

        if self.regs.pc.checked_add(2).is_none() {
            // This may indicate an issue, but is also used in normal behaviour
            // (e.g. m68k Linux or A/UX boot)
            log::warn!(
                "PC reg overflow in prefetch_pump_force. pc={:08X} fetch_addr={:08X} ssp={:08X}",
                self.regs.pc,
                fetch_addr,
                self.regs.ssp(),
            );
        }
        self.regs.pc = self.regs.pc.wrapping_add(2) & ADDRESS_MASK;
        Ok(())
    }

    /// Re-fills the prefetch queue
    pub fn prefetch_refill(&mut self) -> Result<()> {
        while self.prefetch.len() < 2 {
            self.prefetch_pump()?;
        }
        Ok(())
    }

    /// Pops the next word from the prefetch queue, raising a deferred bus
    /// error if the consumed entry is a fault marker.
    fn prefetch_pop(&mut self) -> Result<Word> {
        match self.prefetch.pop_front().unwrap() {
            PrefetchWord::Word(v) => {
                if self.history_enabled {
                    self.history_current.push_raw(&v);
                }
                Ok(v)
            }
            PrefetchWord::Fault(address) => bail!(CpuError::BusError(Group0Details {
                function_code: self.fc_program(),
                ir: 0,
                size: std::mem::size_of::<Word>(),
                atc_fault: false,
                instruction: true,
                read: true,
                address,
                // ??? is this filled correctly later?
                start_pc: 0,
            })),
        }
    }

    /// Fetches a 16-bit value, through the prefetch queue
    pub(in crate::cpu_m68k) fn fetch_pump(&mut self) -> Result<Word> {
        self.prefetch_pump_force()?;
        self.prefetch_pop()
    }

    /// Fetches a 16-bit value from prefetch queue
    pub(in crate::cpu_m68k) fn fetch(&mut self) -> Result<Word> {
        if self.prefetch.is_empty() {
            self.prefetch_pump()?;
        }
        self.prefetch_pop()
    }

    pub fn sync_bus(&mut self) -> Result<()> {
        let cycles_since_last_sync = self.cycles - self.last_bus_sync;
        if cycles_since_last_sync > 0 {
            self.last_bus_sync += cycles_since_last_sync;
            self.bus.tick(cycles_since_last_sync, ())?;
        }

        Ok(())
    }

    /// Executes a single CPU step.
    pub fn step(&mut self) -> Result<()> {
        debug_assert_eq!(self.prefetch.len(), 2);

        self.sync_bus()?;

        self.step_ea_addr = None;
        self.step_exception = false;
        self.step_inhibit_trace = false;
        self.step_over_addr = None;
        self.step_ea_load = None;
        self.exec040 = Exec040::DEFAULT;
        self.step_ea040 = [None; 2];

        if CPU_TYPE >= M68020 {
            self.restart_regs.get_or_insert_default().save(&self.regs);
        }

        // Flag exceptions before executing the instruction to act on them later
        let trace_exception = self.regs.sr.trace() && !self.trace_mask;
        let irq_exception = self.bus.get_irq();

        // Start of instruction execution
        if self.history_enabled {
            self.history_current.pc = self.regs.pc;
            debug_assert!(self.history_current.raw.is_empty());
        }

        let start_cycles = self.cycles;
        let start_pc = self.regs.pc;
        self.step_start_pc = start_pc;
        let opcode = match self.fetch() {
            Ok(o) => o,
            Err(e) => {
                return match e.downcast_ref::<CpuError>() {
                    // Produce deferred bus error now the fetch is no longer
                    // speculative.
                    Some(CpuError::BusError(ae)) => {
                        let mut details = *ae;
                        details.start_pc = start_pc;
                        if self.history_enabled {
                            self.history_current = Default::default();
                        }
                        self.raise_exception(
                            ExceptionGroup::Group0,
                            VECTOR_BUS_ERROR,
                            Some(details),
                        )
                    }
                    _ => Err(e),
                };
            }
        };

        if self.decode_cache[opcode as usize].is_none() {
            match Instruction::try_decode(CPU_TYPE, opcode) {
                Ok(instr) => {
                    self.decode_cache[opcode as usize] = Some(instr);
                }
                Err(e) => {
                    if self.history_enabled {
                        self.history_current = Default::default();
                    }
                    debug!(
                        "Illegal instruction PC {:08X}: {:04X} {:016b} {}",
                        self.regs.pc, opcode, opcode, e
                    );
                    return self.raise_illegal_instruction();
                }
            }
        }

        if self.history_enabled {
            self.history_current.initial_regs = Some(self.regs.clone());
        }

        let instr = self.decode_cache[opcode as usize].clone().unwrap();
        // The M68040 restarts MOVEM and MOVE16, and faults the read-modify-write
        // instructions on their read (M68040UM 8.4.6)
        self.wb040_defer = CPU_TYPE == M68040
            && !matches!(
                instr.mnemonic,
                InstructionMnemonic::MOVEM_mem_w
                    | InstructionMnemonic::MOVEM_mem_l
                    | InstructionMnemonic::MOVE16_inc
                    | InstructionMnemonic::MOVE16_abs
                    | InstructionMnemonic::TAS
                    | InstructionMnemonic::CAS_b
                    | InstructionMnemonic::CAS_w
                    | InstructionMnemonic::CAS_l
            );
        if instr.data & 0xF000 == 0xF000 {
            // A coprocessor instruction starts with a bus cycle to the
            // coprocessor, which its idle clocks stand for
            self.write_credit = 0;
        }
        let mut irq_taken = false;
        let execute_result = if FPU_TYPE != FPU_NONE
            && instr.data & 0xFE00 == 0xF200
            && !self.fpu_wait(instr.data)?
        {
            irq_taken = true;
            Ok(())
        } else {
            self.execute_instruction(&instr)
        };
        self.wb040_defer = false;
        if CPU_TYPE == M68040 && execute_result.is_ok() {
            self.execute040()?;
        }
        if execute_result.is_err() {
            // The instruction restarts and writes again
            self.wb040 = None;
        }

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
                    if let Err(e) = self.raise_exception(
                        ExceptionGroup::Group0,
                        VECTOR_BUS_ERROR,
                        Some(details),
                    ) {
                        let second = e.downcast_ref::<CpuError>().and_then(|c| match c {
                            CpuError::BusError(d) | CpuError::AddressError(d) => Some(*d),
                            _ => None,
                        });
                        log::error!(
                            "DOUBLE FAULT stacking bus error: orig fault@{:08X} fc={} {}; \
                             2nd fault@{:08X} fc={}; SSP={:08X} M={} S={} SR={:04X}; \
                             TC.en={} TC.sre={} CRP={:016X} SRP={:016X} TT0={:08X} TT1={:08X}",
                            details.address,
                            details.function_code,
                            if details.read { "r" } else { "w" },
                            second.map_or(0, |d| d.address),
                            second.map_or(0xFF, |d| d.function_code),
                            *self.regs.ssp(),
                            u8::from(self.regs.sr.m()),
                            u8::from(self.regs.sr.supervisor()),
                            self.regs.sr.sr(),
                            self.regs.pmmu.tc.enable(),
                            self.regs.pmmu.tc.sre(),
                            self.regs.pmmu.crp.0,
                            self.regs.pmmu.srp.0,
                            self.regs.pmmu.tt[0].0,
                            self.regs.pmmu.tt[1].0,
                        );
                        return Err(e);
                    }
                }
                Some(CpuError::IllegalInstruction) => {
                    self.raise_illegal_instruction()?;
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

        if let Some(wb) = self.wb040.as_mut() {
            // The instruction completed with a faulted write, the access error
            // returns after it and carries the pending trace.
            wb.trace = trace_exception && !self.step_inhibit_trace;
            let mut details = wb.details;
            details.ir = instr.data;
            details.start_pc = start_pc;
            self.raise_exception(ExceptionGroup::Group0, VECTOR_BUS_ERROR, Some(details))?;
        }

        // Check pending trace
        if trace_exception && !self.step_inhibit_trace {
            advance_cycles!(self, m68000: 0, m68020: 3, m68030: 6)?;
            self.raise_exception(ExceptionGroup::Group1, VECTOR_TRACE, None)?;
        }

        // Check pending interrupts
        if !irq_taken {
            let level = self.irq_level(irq_exception);
            if level != 0 {
                self.take_irq(level, false)?;
            }
        }

        if !self.breakpoints.is_empty() {
            self.test_pc_breakpoints();
        }

        Ok(())
    }

    /// Tests the breakpoints on the next PC location
    #[cold]
    fn test_pc_breakpoints(&mut self) {
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

    /// Advances by the given amount of cycles, less what overlaps the last write
    pub(in crate::cpu_m68k) fn advance_cycles(&mut self, ticks: Ticks) -> Result<()> {
        let overlap = ticks.min(self.write_credit);
        self.write_credit -= overlap;
        let ticks = ticks - overlap;
        self.cycles += ticks;
        self.bus.cpu_tick(ticks)?;
        Ok(())
    }

    /// Waits for the FPU to finish the arithmetic operation it runs on its own,
    /// before the FPU instruction `opcode`, less the head of the new operation on
    /// the 68882. Meanwhile the FPU answers "come again, interrupts allowed", so an
    /// interrupt that comes first is taken with a mid-instruction frame
    /// (MC68881/MC68882UM 5.1.1.2, 6.2.5). Returns false in that case. RTE then
    /// starts the instruction again, which has not changed anything yet.
    fn fpu_wait(&mut self, opcode: Word) -> Result<bool> {
        // About one read of the response CIR between the interrupt checks
        const POLL: Ticks = 4;

        if self.cycles >= self.fpu_free_at {
            self.fpu_busy_regs = 0;
        }
        let free_at = self.fpu_free_at.saturating_sub(self.fpu_head(opcode));
        while self.cycles < free_at {
            let irq = self.bus.get_irq();
            let level = self.irq_level(irq);
            if level != 0 {
                self.step_inhibit_trace = true;
                self.take_irq(level, true)?;
                return Ok(false);
            }
            self.advance_cycles((free_at - self.cycles).min(POLL))?;
            self.sync_bus()?;
        }
        Ok(true)
    }

    /// Sets the program counter and flushes the prefetch queue
    pub fn set_pc(&mut self, pc: Address) -> Result<()> {
        self.prefetch.clear();
        self.holding_valid = false;
        self.regs.pc = pc.wrapping_sub(4) & ADDRESS_MASK;
        Ok(())
    }

    /// Sets SR, masking CPU model dependent bits accordingly
    pub fn set_sr(&mut self, sr: Word) {
        self.regs.sr.set_sr(match CPU_TYPE {
            M68000 => sr & M68000_SR_MASK,
            M68020 => sr & M68020_SR_MASK,
            M68030 => sr & M68030_SR_MASK,
            M68040 => sr & M68040_SR_MASK,
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
        // "If the instruction is not executed because the instruction is illegal or privileged,
        // the trace exception does not occur."
        self.step_inhibit_trace = true;

        warn!("Illegal instruction at PC ${:08X}", self.regs.pc);
        advance_cycles!(self, m68020: 1, others: 4)?;
        account_040_time!(self, timing040::TRAP);
        self.raise_exception(ExceptionGroup::Group1, VECTOR_ILLEGAL, None)?;
        Ok(())
    }

    /// Raises a privilege violation exception
    fn raise_privilege_violation(&mut self) -> Result<()> {
        // "If the instruction is not executed because the instruction is illegal or privileged,
        // the trace exception does not occur."
        self.step_inhibit_trace = true;

        advance_cycles!(self, m68020: 1, others: 4)?;
        account_040_time!(self, timing040::TRAP);
        self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None)?;
        Ok(())
    }

    /// Returns the level of the interrupt to take for the interrupt request `irq`,
    /// or 0 if there is none to take
    fn irq_level(&mut self, irq: Option<u8>) -> u8 {
        // NMI is edge-triggered
        if irq != Some(7) {
            self.in_nmi = false;
        }
        match irq {
            Some(7) => {
                if !self.in_nmi {
                    self.in_nmi = true;
                    7
                } else {
                    0
                }
            }
            Some(level) if level > self.regs.sr.int_prio_mask() => level,
            _ => 0,
        }
    }

    /// Takes the interrupt of `level`, during the dialog of an FPU instruction if
    /// `mid_instruction`
    fn take_irq(&mut self, level: u8, mid_instruction: bool) -> Result<()> {
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
            mid_instruction,
        )
    }

    /// Raises an IRQ to be executed next
    fn raise_irq(&mut self, level: u8, vector: Address, mid_instruction: bool) -> Result<()> {
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
                // The coprocessor mid-instruction frame (format $9) has the scanPC,
                // past the command word, and the address of the instruction
                // (MC68020UM figure 7-43)
                let (pc, format) = if mid_instruction {
                    *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(20);
                    (
                        self.step_start_pc.wrapping_add(4) & ADDRESS_MASK,
                        0b1001_0000_0000_0000u16,
                    )
                } else {
                    *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(8);
                    (self.regs.pc, 0b0000_0000_0000_0000u16)
                };
                // Idle cycles and the interrupt acknowledge cycle
                advance_cycles!(self, m68000: 0, m68020: 7, m68030: 9)?;

                self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                self.write_ticks(self.regs.ssp().wrapping_add(2), (pc >> 16) as u16)?;
                self.write_ticks(self.regs.ssp().wrapping_add(4), pc as u16)?;
                self.write_ticks(self.regs.ssp().wrapping_add(6), format | (vector as u16))?;
                if mid_instruction {
                    self.write_ticks(self.regs.ssp().wrapping_add(8), self.step_start_pc)?;
                    // Internal registers
                    self.write_ticks(self.regs.ssp().wrapping_add(0x0C), 0u32)?;
                    self.write_ticks(self.regs.ssp().wrapping_add(0x10), 0u32)?;
                }
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
        advance_cycles!(self, m68000: 2)?; // 2x idle
        self.prefetch_pump()?;
        if CPU_TYPE == M68040 {
            self.advance_cycles(timing040::EXCEPTION)?;
        }

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

        // Stacking faults are not deferred
        self.wb040_defer = false;

        // Resume in supervisor mode
        self.regs.sr.set_supervisor(true);
        self.regs.sr.set_trace(false);

        // Write exception stack frame
        match group {
            ExceptionGroup::Group0 => {
                self.step_exception = true;
                // A bus or address error inhibits the pending trace exception
                // for the aborted instruction (M68000PRM 6.2.4, M68020UM 6.4,
                // M68030UM 8.4.3). The trace will fire naturally after the
                // instruction replays on RTE.
                self.step_inhibit_trace = true;

                advance_cycles!(self, m68000: 8)?; // idle
                let details = details.expect("Address error details not passed");

                match CPU_TYPE {
                    M68040 if vector == VECTOR_ADDRESS_ERROR => {
                        if let Some(regs) = self.restart_regs.take() {
                            saved_sr = regs.sr.sr();
                            regs.restore(&mut self.regs);
                            self.regs.sr.set_supervisor(true);
                            self.regs.sr.set_trace(false);
                        } else {
                            log::error!(
                                "Cannot reset registers for stacking an address error frame"
                            );
                        }

                        // Six word stack frame (format $2) with the address,
                        // bit 0 cleared (M68040UM 8.4.6.7)
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(12);
                        let ssp = *self.regs.ssp();
                        self.write_ticks(ssp.wrapping_add(0), saved_sr)?;
                        self.write_ticks(ssp.wrapping_add(2), details.start_pc)?;
                        self.write_ticks(
                            ssp.wrapping_add(6),
                            0b0010_0000_0000_0000 | (vector as u16),
                        )?;
                        self.write_ticks(ssp.wrapping_add(8), details.address & !1)?;
                    }
                    M68040 => {
                        // A faulted write of a completed instruction waits in a
                        // write-back slot for the handler, which returns after
                        // the instruction. Other faults restart the instruction.
                        let wb = self.wb040.take();
                        let pc = if wb.is_some() {
                            self.regs.pc
                        } else {
                            if let Some(regs) = self.restart_regs.take() {
                                saved_sr = regs.sr.sr();
                                regs.restore(&mut self.regs);
                                self.regs.sr.set_supervisor(true);
                                self.regs.sr.set_trace(false);
                            } else {
                                log::error!(
                                    "Cannot reset registers for stacking an access error frame"
                                );
                            }
                            details.start_pc
                        };
                        let trace = wb.is_some_and(|wb| wb.trace);
                        let size: u16 = match details.size {
                            1 => 0b01,
                            2 => 0b10,
                            16 => 0b11,
                            // Long, and the default when the size is
                            // not known (MMU faults)
                            _ => 0b00,
                        };

                        // Access error stack frame (format $7), M68040UM 8.4.6.
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(0x3C);
                        let ssp = *self.regs.ssp();

                        self.write_ticks(ssp.wrapping_add(0x00), saved_sr)?;
                        self.write_ticks(ssp.wrapping_add(0x02), pc)?;
                        self.write_ticks(
                            ssp.wrapping_add(0x06),
                            0b0111_0000_0000_0000 | (vector as u16),
                        )?;
                        // Effective address: the traced instruction with a
                        // pending trace, else the faulted access
                        self.write_ticks(
                            ssp.wrapping_add(0x08),
                            if trace {
                                details.start_pc
                            } else {
                                details.address
                            },
                        )?;
                        // Special status word
                        self.write_ticks(
                            ssp.wrapping_add(0x0C),
                            *Ssw040::default()
                                .with_tm(details.function_code)
                                // A 16-byte operand is a MOVE16 line transfer,
                                // except a cache push, which reports as a write
                                // with TM=0, TT=0 (MC68040UM 8.4.6)
                                .with_tt(u8::from(details.size == 16 && details.function_code != 0))
                                .with_rw(details.read)
                                .with_atc(details.atc_fault)
                                .with_ma(wb.is_some_and(|wb| wb.ma))
                                .with_ct(trace)
                                .with_size(size as u8),
                        )?;

                        // Write-back slots, WB3 to WB1: status, address, data.
                        // WB2 holds a write the MMU faulted (register aligned
                        // data), WB1 one faulted on the bus (data on its byte
                        // lanes, M68040UM table 8-5).
                        let mut slots = [(0u16, 0u32, 0u32); 3];
                        if let Some(wb) = wb {
                            let status = 0x80 | (size << 5) | u16::from(details.function_code);
                            slots[if details.atc_fault { 1 } else { 2 }] = (
                                status,
                                details.address,
                                if details.atc_fault {
                                    wb.data
                                } else {
                                    (wb.data << (32 - 8 * details.size as u32))
                                        .rotate_right(8 * (details.address & 3))
                                },
                            );
                        }
                        for (i, &(status, _, _)) in slots.iter().enumerate() {
                            self.write_ticks(ssp.wrapping_add(0x0E + 2 * i as Address), status)?;
                        }
                        // Fault address
                        self.write_ticks(ssp.wrapping_add(0x14), details.address)?;
                        for (i, &(_, addr, data)) in slots.iter().enumerate() {
                            self.write_ticks(ssp.wrapping_add(0x18 + 8 * i as Address), addr)?;
                            self.write_ticks(ssp.wrapping_add(0x1C + 8 * i as Address), data)?;
                        }
                        // Push data, long words 1 to 3
                        for offset in (0x30..0x3C).step_by(4) {
                            self.write_ticks(ssp.wrapping_add(offset), 0u32)?;
                        }
                    }
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
                            regs.restore(&mut self.regs);
                            self.regs.sr.set_supervisor(true);
                            self.regs.sr.set_trace(false);
                        } else {
                            log::error!("Cannot reset registers for stacking a bus error frame");
                        }

                        if self.regs.pc == details.start_pc && self.prefetch.len() == 2 {
                            // Bus error at instruction boundary
                            advance_cycles!(self, m68000: 0, m68020: 6, m68030: 10)?;
                            *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(32);
                            self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                            self.write_ticks(self.regs.ssp().wrapping_add(0x02), details.start_pc)?;
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x06),
                                0b1010_0000_0000_0000 | (vector as u16),
                            )?;
                            // Internal register and special status register
                            // TODO size
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x08),
                                Long::from(
                                    *Group0Ssw::default()
                                        .with_read(details.read)
                                        .with_df(true)
                                        .with_function_code(details.function_code)
                                        .with_size(match details.size {
                                            1 => 1,
                                            2 => 2,
                                            3 => 3,
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
                                ),
                            )?;
                            // Instruction pipe stages C and B
                            self.write_ticks(self.regs.ssp().wrapping_add(0x0C), 0u32)?;
                            // Data cycle fault address
                            self.write_ticks(self.regs.ssp().wrapping_add(0x10), details.address)?;
                            // Internal registers
                            self.write_ticks(self.regs.ssp().wrapping_add(0x14), 0u32)?;
                            // Data output buffer
                            self.write_ticks(self.regs.ssp().wrapping_add(0x18), 0u32)?;
                            // Internal registers
                            self.write_ticks(self.regs.ssp().wrapping_add(0x1C), 0u32)?;
                        } else {
                            advance_cycles!(self, m68030: 8)?;
                            *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(92);
                            self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                            self.write_ticks(self.regs.ssp().wrapping_add(0x02), details.start_pc)?;
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x06),
                                0b1011_0000_0000_0000 | (vector as u16),
                            )?;
                            // Internal register and special status register
                            // TODO size
                            self.write_ticks(
                                self.regs.ssp().wrapping_add(0x08),
                                Long::from(
                                    *Group0Ssw::default()
                                        .with_read(details.read)
                                        .with_df(true)
                                        .with_function_code(details.function_code)
                                        .with_size(match details.size {
                                            1 => 1,
                                            2 => 2,
                                            3 => 3,
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
                                ),
                            )?;
                            // Instruction pipe stages C and B
                            self.write_ticks(self.regs.ssp().wrapping_add(0x0C), 0u32)?;
                            // Data cycle fault address
                            self.write_ticks(self.regs.ssp().wrapping_add(0x10), details.address)?;

                            // More internal stuff
                            for i in (0x14..0x5C).step_by(4) {
                                self.write_ticks(self.regs.ssp().wrapping_add(i), 0u32)?;
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
                // The address of the six word frame (format $2): of the
                // instruction that caused the exception (M68030UM table 8-6,
                // M68040UM 8.4.3), or the calculated effective address for an
                // unimplemented floating-point instruction (M68040UM 9.6.1).
                // TRAPcc and cpTRAPcc share the TRAPV vector, CHK2 the CHK one.
                let fpu_unimp = std::mem::take(&mut self.step_fpu_unimp);
                let six_word = match vector {
                    _ if CPU_TYPE < M68020 => None,
                    VECTOR_DIV_ZERO | VECTOR_CHK | VECTOR_TRAPV | VECTOR_TRACE => {
                        Some(self.step_start_pc)
                    }
                    VECTOR_LINEF if fpu_unimp => Some(self.step_ea_addr.unwrap_or(0)),
                    _ => None,
                };
                match (CPU_TYPE, six_word) {
                    (M68000, _) => {
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(6);
                        self.write_ticks(self.regs.ssp().wrapping_add(4), pc as u16)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(0), saved_sr)?;
                        self.write_ticks(self.regs.ssp().wrapping_add(2), (pc >> 16) as u16)?;
                    }
                    (_, Some(address)) => {
                        // The PC takes two writes, five in all as the
                        // exception tables count
                        *self.regs.ssp_mut() = self.regs.ssp().wrapping_sub(12);
                        let ssp = *self.regs.ssp();
                        self.write_ticks(ssp.wrapping_add(0), saved_sr)?;
                        self.write_ticks(ssp.wrapping_add(2), (pc >> 16) as u16)?;
                        self.write_ticks(ssp.wrapping_add(4), pc as u16)?;
                        self.write_ticks(
                            ssp.wrapping_add(6),
                            0b0010_0000_0000_0000 | (vector as u16),
                        )?;
                        self.write_ticks(ssp.wrapping_add(8), address)?;
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
        advance_cycles!(self, m68000: 2)?; // 2x idle
        self.prefetch_pump()?;
        if CPU_TYPE == M68040 {
            self.advance_cycles(timing040::EXCEPTION)?;
        }

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
            InstructionMnemonic::NOP => {
                account_040_time!(self, timing040::NOP);
                Ok(())
            }
            // Only the 68040 has MOVE16: this also keeps the line transfers
            // out of the other CPUs at compile time
            InstructionMnemonic::MOVE16_inc if CPU_TYPE >= M68040 => self.op_move16_inc(instr),
            InstructionMnemonic::MOVE16_abs if CPU_TYPE >= M68040 => self.op_move16_abs(instr),
            InstructionMnemonic::MOVE16_inc | InstructionMnemonic::MOVE16_abs => {
                self.op_linef(instr)
            }
            InstructionMnemonic::CINV | InstructionMnemonic::CPUSH => self.op_cxxx(instr),
            InstructionMnemonic::PFLUSH040 => self.op_pflush040(instr),
            InstructionMnemonic::PTEST040 => self.op_ptest040(instr),
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
            InstructionMnemonic::MOVES_l => self.op_moves::<Long>(instr),
            InstructionMnemonic::MOVES_w => self.op_moves::<Word>(instr),
            InstructionMnemonic::MOVES_b => self.op_moves::<Byte>(instr),
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
            InstructionMnemonic::PACK => self.op_pack(instr),
            InstructionMnemonic::UNPK => self.op_unpk(instr),
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
            InstructionMnemonic::STOP => self.op_stop(instr),
            InstructionMnemonic::TRAPV => self.op_trapv(instr),
            InstructionMnemonic::TRAPcc => self.op_trapcc::<0>(instr),
            InstructionMnemonic::TRAPcc_w => self.op_trapcc::<1>(instr),
            InstructionMnemonic::TRAPcc_l => self.op_trapcc::<2>(instr),
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

                advance_cycles!(self, m68020: 1, others: 4)?;
                account_040_time!(self, timing040::TRAP);
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
            InstructionMnemonic::CMP2_b => self.op_cmp2::<Byte>(instr),
            InstructionMnemonic::CMP2_w => self.op_cmp2::<Word>(instr),
            InstructionMnemonic::CMP2_l => self.op_cmp2::<Long>(instr),

            // FPU ---------------------------------------------------------------------------------
            InstructionMnemonic::FNOP
            | InstructionMnemonic::FSAVE
            | InstructionMnemonic::FRESTORE
            | InstructionMnemonic::FOP_000
            | InstructionMnemonic::FBcc_l
            | InstructionMnemonic::FBcc_w
            | InstructionMnemonic::FScc_b
            | InstructionMnemonic::FDBcc
            | InstructionMnemonic::FTRAPcc
            | InstructionMnemonic::FTRAPcc_w
            | InstructionMnemonic::FTRAPcc_l
                if FPU_TYPE == FPU_NONE =>
            {
                self.op_linef(instr)
            }
            InstructionMnemonic::FNOP => self.op_fnop(instr),
            InstructionMnemonic::FSAVE => self.op_fsave(instr),
            InstructionMnemonic::FRESTORE => self.op_frestore(instr),
            InstructionMnemonic::FOP_000 => self.op_f000(instr),
            InstructionMnemonic::FBcc_l => self.op_fbcc::<true>(instr),
            InstructionMnemonic::FBcc_w => self.op_fbcc::<false>(instr),
            InstructionMnemonic::FScc_b => self.op_fscc(instr),
            InstructionMnemonic::FDBcc => self.op_fdbcc(instr),
            InstructionMnemonic::FTRAPcc => self.op_ftrapcc::<0>(instr),
            InstructionMnemonic::FTRAPcc_w => self.op_ftrapcc::<1>(instr),
            InstructionMnemonic::FTRAPcc_l => self.op_ftrapcc::<2>(instr),

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

        advance_cycles!(self, m68020: 1, others: 4)?;
        account_040_time!(self, timing040::TRAP);
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
        advance_cycles!(self, m68000: 0, others: 2)?;
        account_040_time!(self, timing040::SWAP);

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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => 4,
                (_, Direction::Right, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::ALU, instr.get_addr_mode()?);

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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister, _, 4) => 4,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::ALU_IMMEDIATE, instr.get_addr_mode()?);

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
        advance_cycles!(self, all: 8)?;
        if CPU_TYPE == M68000 {
            self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        }
        self.prefetch_pump()?;

        account_040_time!(self, timing040::BITWISE_CCR);
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
        advance_cycles!(self, all: 8)?;
        if CPU_TYPE == M68000 {
            self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        }
        self.prefetch_pump()?;

        account_040_time!(self, timing040::BITWISE_SR);
        Ok(())
    }

    /// TRAP
    fn op_trap(&mut self, instr: &Instruction) -> Result<()> {
        // Offset PC correctly for exception stack frame for TRAP
        self.regs.pc = self.regs.pc.wrapping_add(2);

        advance_cycles!(self, m68020: 1, others: 4)?;
        account_040_time!(self, timing040::TRAP);
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
            advance_cycles!(self, m68000: 0, others: 2)?;
            account_040_time!(self, timing040::TRAPCC_NOT_TAKEN);
            return Ok(());
        }

        advance_cycles!(self, m68000: 0, m68020: 1, m68030: 4)?;
        account_040_time!(self, timing040::TRAPCC_TAKEN);
        self.raise_exception(ExceptionGroup::Group2, VECTOR_TRAPV, None)
    }

    /// TRAPcc (68020+) - OPERAND_WORDS is 0, 1 (word) or 2 (long)
    fn op_trapcc<const OPERAND_WORDS: usize>(&mut self, instr: &Instruction) -> Result<()> {
        let taken = self.cc(instr.get_cc());

        // Consume the immediate operand from the prefetch queue and refill.
        // After this, regs.pc points to the instruction following TRAPcc, which
        // is also the address the exception stack frame must capture.
        for _ in 0..OPERAND_WORDS {
            let _: Word = self.fetch_pump()?;
        }
        self.prefetch_pump()?;

        if !taken {
            advance_cycles!(self, m68000: 0, others: 2)?;
            account_040_time!(self, timing040::TRAPCC_NOT_TAKEN);
            return Ok(());
        }

        // The 68020 takes the same time with or without the operand words
        if OPERAND_WORDS == 0 {
            advance_cycles!(self, m68000: 0, m68020: 1, m68030: 4)?;
        } else {
            advance_cycles!(self, m68030: 4)?;
        }
        account_040_time!(self, timing040::TRAPCC_TAKEN);
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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => 4,
                (AddressingMode::AddressRegister, _, 4) => 4,
                (_, Direction::Right, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::ALU, instr.get_addr_mode()?);

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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister, _, 4) => 4,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::ALU_IMMEDIATE, instr.get_addr_mode()?);

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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister, _, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::NEG, instr.get_addr_mode()?);

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
        advance_cycles!(
            self,
            m68000: match (instr.get_addr_mode()?, std::mem::size_of::<T>()) {
                (AddressingMode::DataRegister, 4) => 4,
                (AddressingMode::AddressRegister, 4) => 4,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::ADDQ_SUBQ, instr.get_addr_mode()?);

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
            .expand_signed()
            .cast_unsigned();
        self.ea_commit();
        let a: Long = self.regs.read_a(instr.get_op1());
        let (result, _) = calcfn(a, b, self.regs.sr);

        // Flags are not changed
        self.prefetch_pump()?;
        self.regs.write_a::<Long>(instr.get_op1(), result);

        // Idle cycles. The 68030 manual gives ADDA.W/SUBA.W 4 clocks and the .L
        // forms 2, the 68020 manual has one 2-clock row for both sizes. It is not
        // known whether the 68020 really differs; we follow each manual.
        advance_cycles!(
            self,
            m68000: match (instr.get_addr_mode()?, std::mem::size_of::<T>()) {
                (
                    AddressingMode::AddressRegister
                    | AddressingMode::DataRegister
                    | AddressingMode::Immediate,
                    _,
                ) => 4,
                (_, 2) => 4,
                (_, 4) => 2,
                _ => unreachable!(),
            },
            m68020: 0,
            m68030: if std::mem::size_of::<T>() == 2 { 2 } else { 0 },
        )?;
        account_040_time!(
            self,
            if matches!(
                instr.mnemonic,
                InstructionMnemonic::SUBA_l | InstructionMnemonic::SUBA_w
            ) {
                timing040::SUBA
            } else {
                timing040::ADDA
            },
            instr.get_addr_mode()?,
        );

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
            (AddressingMode::IndirectPreDec, 4) if CPU_TYPE == M68000 => {
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
                // MC68020UM 8.2.10, MC68030UM 11.6.10
                advance_cycles!(self, m68000: 2, others: 1)?;
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
            (AddressingMode::IndirectPreDec, 4) if CPU_TYPE == M68000 => {
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
        advance_cycles!(
            self,
            m68000: match (instr.get_addr_mode_x()?, sz) {
                (AddressingMode::DataRegister, 4) => 4,
                _ => 0,
            },
        )?;
        account_040_time!(
            self,
            if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
                timing040::ADDX_DN
            } else {
                timing040::ADDX_PREDEC
            },
        );

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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister | AddressingMode::Immediate, _, 4) => 2,
                (AddressingMode::AddressRegister, _, 4) => 2,
                (_, Direction::Right, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::CMP, instr.get_addr_mode()?);

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
        advance_cycles!(
            self,
            m68000: match (
                instr.get_addr_mode()?,
                instr.get_direction(),
                std::mem::size_of::<T>(),
            ) {
                (AddressingMode::DataRegister, _, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::CMPI, instr.get_addr_mode()?);

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
        advance_cycles!(self, m68000: 0, m68020: 1, m68030: 2)?;

        account_040_time!(self, timing040::CMPM);
        Ok(())
    }

    /// CMPA
    fn op_cmp_address<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let b = self
            .read_ea::<T>(instr, instr.get_op2())?
            .expand_signed()
            .cast_unsigned();
        let a: Long = self.regs.read_a(instr.get_op1());

        let (_, ccr) = Self::alu_sub(a, b, self.regs.sr);

        let old_x = self.regs.sr.x();
        self.regs.sr.set_ccr(ccr);
        self.regs.sr.set_x(old_x);

        self.prefetch_pump()?;
        advance_cycles!(self, all: 2)?; // 2x idle
        account_040_time!(self, timing040::CMPA_L, instr.get_addr_mode()?);

        Ok(())
    }

    /// MULU
    fn op_mulu(&mut self, instr: &Instruction) -> Result<()> {
        let a = self.regs.read_d::<Word>(instr.get_op1()) as Long;
        let b = self.read_ea::<Word>(instr, instr.get_op2())? as Long;
        let result = a.wrapping_mul(b);

        self.prefetch_pump()?;

        // Computation time. On the 68020/68030 MUL and DIV take the maximum times
        // from the manuals (MC68020UM 8.2.8, MC68030UM 11.6.8). The real times
        // depend on the operands, but there is no data on how. Other emulators use
        // fixed times as well, and the few measurements found online roughly match.
        advance_cycles!(
            self,
            m68000: 34 + (b.count_ones() as Ticks) * 2,
            m68020: 25,
            m68030: 26,
        )?;

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & 0x8000_0000 != 0);
        self.regs.sr.set_z(result == 0);

        self.regs.write_d(instr.get_op1(), result);

        account_040_time!(self, timing040::MULU_W, instr.get_addr_mode()?);
        Ok(())
    }

    /// MULS (Word)
    fn op_muls_w(&mut self, instr: &Instruction) -> Result<()> {
        let a = self.regs.read_d::<Word>(instr.get_op1()) as i16 as i32;
        let b = self.read_ea::<Word>(instr, instr.get_op2())? as i16 as i32;
        let result = a.wrapping_mul(b) as Long;

        self.prefetch_pump()?;

        // Computation time (68020/68030: maximum from the manuals, see op_mulu)
        advance_cycles!(
            self,
            m68000: 34 + (((b << 1) ^ b).count_ones() as Ticks) * 2,
            m68020: 25,
            m68030: 26,
        )?;

        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_n(result & 0x8000_0000 != 0);
        self.regs.sr.set_z(result == 0);

        self.regs.write_d(instr.get_op1(), result);

        account_040_time!(self, timing040::MULS_W, instr.get_addr_mode()?);
        Ok(())
    }

    /// DIVU
    fn op_divu(&mut self, instr: &Instruction) -> Result<()> {
        let mut dividend = self.regs.read_d::<Long>(instr.get_op1());
        let mut divisor = self.read_ea::<Word>(instr, instr.get_op2())? as Long;

        if divisor == 0 {
            // Division by zero. The manuals have no time for this on the 68020/68030;
            // take that of TRAPV, which stacks the same frame.
            advance_cycles!(self, m68000: 8, m68020: 3, m68030: 6)?;
            account_040_time!(self, timing040::DIV_W_ZERO);
            self.regs.sr.set_n(false);
            self.regs.sr.set_c(false);
            self.regs.sr.set_z(false);
            self.regs.sr.set_v(false);

            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_DIV_ZERO, None);
        }

        account_040_time!(self, timing040::DIV_W, instr.get_addr_mode()?);

        let result = dividend / divisor;
        let result_rem = dividend % divisor;

        self.regs.sr.set_c(false);
        self.regs.sr.set_z(false);
        // Maximum from the manuals on the 68020/68030, see op_mulu
        advance_cycles!(self, m68000: 6, others: 42)?;
        if result > Word::MAX.into() {
            // Overflow
            self.regs.sr.set_v(true);
            self.regs.sr.set_n(true);
            self.prefetch_pump()?;

            return Ok(());
        }

        // Simulate the cycle time
        if CPU_TYPE == M68000 {
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
            // Division by zero. The manuals have no time for this on the 68020/68030;
            // take that of TRAPV, which stacks the same frame.
            advance_cycles!(self, m68000: 8, m68020: 3, m68030: 6)?;
            account_040_time!(self, timing040::DIV_W_ZERO);
            self.regs.sr.set_n(false);
            self.regs.sr.set_c(false);
            self.regs.sr.set_z(false);
            self.regs.sr.set_v(false);

            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_DIV_ZERO, None);
        }

        account_040_time!(self, timing040::DIV_W, instr.get_addr_mode()?);

        // $80000000 / -1 doesn't fit, it ends up in the overflow below
        let result = dividend.wrapping_div(divisor);
        let result_rem = dividend.wrapping_rem(divisor);

        self.regs.sr.set_c(false);
        self.regs.sr.set_z(false);
        // Maximum from the manuals on the 68020/68030, see op_mulu
        advance_cycles!(
            self,
            m68000: if dividend < 0 { 10 } else { 8 },
            others: 54,
        )?;
        if dividend.wrapping_abs() >= (divisor.wrapping_abs() << 16) && divisor != i16::MIN as i32 {
            // Overflow (detected before calculation)
            advance_cycles!(self, m68000: 4)?;

            self.regs.sr.set_v(true);
            self.regs.sr.set_n(true);
            self.prefetch_pump()?;

            return Ok(());
        }

        // Simulate the cycle time: +2 for a negative divisor, +4 for a positive
        // divisor and a negative dividend, and 2 per zero in the top 15 bits
        advance_cycles!(
            self,
            m68000: {
                let sign = if divisor < 0 {
                    2
                } else if dividend < 0 {
                    4
                } else {
                    0
                };
                let zeroes = ((result.wrapping_abs() as u16) | 1).count_zeros() as Ticks;
                sign + 108 + zeroes * 2
            },
        )?;

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
                advance_cycles!(self, all: 2)?;
                if let Some(cf) = calcfn {
                    // The 68030 takes two more clocks than the 68020 to change
                    // the bit (MC68020UM 8.2.13, MC68030UM 11.6.13)
                    advance_cycles!(self, m68030: 2)?;
                    self.write_ea(instr, instr.get_op2(), cf(val, bit))?;
                    if bitnum % 32 > 15 {
                        advance_cycles!(self, m68000: 2)?;
                    }
                    if instr.mnemonic == InstructionMnemonic::BCLR_dn
                        || instr.mnemonic == InstructionMnemonic::BCLR_imm
                    {
                        // :'(
                        advance_cycles!(self, m68000: 2)?;
                    }
                }
            }
            _ => {
                let val: Byte = self.read_ea(instr, instr.get_op2())?;
                let bit = 1_u8 << (bitnum % 8);
                self.regs.sr.set_z(val & bit == 0);
                self.prefetch_pump()?;
                if let Some(cf) = calcfn {
                    advance_cycles!(self, m68030: 2)?;
                    self.write_ea(instr, instr.get_op2(), cf(val as Long, bit.into()) as Byte)?;
                }
            }
        };

        // Idle cycles
        if instr.get_addr_mode()? == AddressingMode::Immediate {
            advance_cycles!(self, all: 2)?;
        }

        account_040_time!(
            self,
            self.exec040_ea(
                if calcfn.is_some() {
                    &timing040::BIT_CHANGE
                } else {
                    &timing040::BTST
                },
                instr.get_addr_mode()?,
            )
            // One more clock with the bit number in a register
            .plus(u16::from(!IMM)),
        );
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
            .wrapping_add_signed(instr.get_displacement())
            & ADDRESS_MASK;

        if instr.get_direction_movep() == Direction::Right {
            // To bus
            let data = self.regs.read_d::<T>(instr.get_op1()).to_be_bytes();

            for (i, b) in data.as_ref().iter().cloned().enumerate() {
                let b_addr = addr.wrapping_add((i * 2) as Address);
                self.write_ticks::<Byte>(b_addr, b)?;
            }
            advance_cycles!(self, m68000: 0, m68020: 1, m68030: 2)?;
        } else {
            // From bus
            let mut data = [0; N];
            for (i, d) in data.iter_mut().enumerate().take(N) {
                let b_addr = addr.wrapping_add((i * 2) as Address);
                *d = self.read_ticks::<Byte>(b_addr)?;
            }

            self.regs
                .write_d::<T>(instr.get_op1(), T::from_be_bytes(&data));
            advance_cycles!(self, m68000: 0, others: 2)?;
        }

        account_040_time!(
            self,
            match (instr.get_direction_movep() == Direction::Right, N) {
                (true, 2) => timing040::MOVEP_W_TO_MEM,
                (true, _) => timing040::MOVEP_L_TO_MEM,
                (false, 2) => timing040::MOVEP_W_TO_DN,
                (false, _) => timing040::MOVEP_L_TO_DN,
            },
        );
        Ok(())
    }

    /// MOVEA
    fn op_movea<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let value: T = self.read_ea(instr, instr.get_op2())?;
        self.regs.write_a(instr.get_op1(), value);
        // MOVEA.W sign-extends memory operands in one more clock
        let mode = instr.get_addr_mode()?;
        account_040_time!(
            self,
            self.exec040_ea(&timing040::MOVEA_L, mode).plus(u16::from(
                std::mem::size_of::<T>() == 2
                    && !matches!(
                        mode,
                        AddressingMode::DataRegister | AddressingMode::Immediate
                    )
            )),
        );
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

        account_040_time!(
            self,
            self.exec040_move(instr.get_addr_mode()?, instr.get_addr_mode_left()?),
        );
        Ok(())
    }

    /// MOVEfromSR
    fn op_move_from_sr(&mut self, instr: &Instruction) -> Result<()> {
        if CPU_TYPE >= M68010 && !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        let value = self.regs.sr.sr();

        if CPU_TYPE == M68000 {
            // Discarded read, prefetch
            self.read_ea::<Word>(instr, instr.get_op2())?;
            self.prefetch_pump()?;
        }

        self.write_ea(instr, instr.get_op2(), value)?;

        // Idle cycles
        let register = matches!(
            instr.get_addr_mode()?,
            AddressingMode::DataRegister | AddressingMode::AddressRegister
        );
        advance_cycles!(self, all: if register { 2 } else { 0 })?;

        account_040_time!(self, timing040::MOVE_FROM_SR, instr.get_addr_mode()?);
        Ok(())
    }

    /// MOVEtoSR
    fn op_move_to_sr(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }
        let value: Word = self.read_ea(instr, instr.get_op2())?;

        if CPU_TYPE == M68000 {
            // Idle cycles and discarded read
            self.advance_cycles(4)?;
            self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
        }
        advance_cycles!(self, m68000: 0, others: 6)?;

        self.set_sr(value);
        account_040_time!(self, timing040::MOVE_TO_SR, instr.get_addr_mode()?);
        Ok(())
    }

    /// MOVEtoCCR
    fn op_move_to_ccr(&mut self, instr: &Instruction) -> Result<()> {
        let value: Word = self.read_ea(instr, instr.get_op2())?;

        // Idle cycles + discarded read
        if CPU_TYPE == M68000 {
            self.advance_cycles(4)?;
            self.read_ticks::<Word>(self.regs.pc.wrapping_add(2) & ADDRESS_MASK)?;
            self.prefetch_pump()?;
        }
        advance_cycles!(self, m68000: 0, others: 2)?;

        self.regs.sr.set_ccr(value as Byte);
        account_040_time!(self, timing040::MOVE_TO_CCR, instr.get_addr_mode()?);
        Ok(())
    }

    /// MOVEfromCCR
    fn op_move_from_ccr(&mut self, instr: &Instruction) -> Result<()> {
        let dn = instr.get_addr_mode()? == AddressingMode::DataRegister;
        advance_cycles!(self, all: if dn { 2 } else { 0 })?;
        self.write_ea::<Word>(instr, instr.get_op2(), self.regs.sr.ccr().into())?;
        self.prefetch_pump()?;

        account_040_time!(self, timing040::MOVE_FROM_CCR, instr.get_addr_mode()?);
        Ok(())
    }

    /// MOVEtoUSP
    fn op_move_to_usp(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }
        let value: Address = self.regs.read_a(instr.get_op2());

        self.regs.usp = value;
        advance_cycles!(self, m68030: 2)?;
        account_040_time!(self, timing040::MOVE_TO_USP);
        Ok(())
    }

    /// MOVEfromUSP
    fn op_move_from_usp(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }
        let value: Address = self.regs.usp;

        self.regs.write_a(instr.get_op2(), value);
        advance_cycles!(self, m68030: 2)?;
        account_040_time!(self, timing040::MOVE_FROM_USP);
        Ok(())
    }

    /// CLR
    fn op_clr<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        if CPU_TYPE == M68000 {
            self.read_ea::<T>(instr, instr.get_op2())?;
            self.prefetch_pump()?;
        }

        self.regs.sr.set_n(false);
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_z(true);
        self.write_ea(instr, instr.get_op2(), T::zero())?;

        // Idle cycles
        advance_cycles!(
            self,
            m68000: match (instr.get_addr_mode()?, std::mem::size_of::<T>()) {
                (AddressingMode::DataRegister | AddressingMode::AddressRegister, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::CLR, instr.get_addr_mode()?);

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
        advance_cycles!(
            self,
            m68000: match (instr.get_addr_mode()?, std::mem::size_of::<T>()) {
                (AddressingMode::DataRegister | AddressingMode::AddressRegister, 4) => 2,
                _ => 0,
            },
        )?;
        account_040_time!(self, timing040::NEG, instr.get_addr_mode()?);

        Ok(())
    }

    /// EXT
    fn op_ext<T: CpuSized, U: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        // T: dest type, U: src type
        let value: U = self.read_ea(instr, instr.get_op2())?;
        let result = T::chop(value.expand_signed().cast_unsigned());

        self.regs.sr.set_n(result & T::msb() != T::zero());
        self.regs.sr.set_v(false);
        self.regs.sr.set_c(false);
        self.regs.sr.set_z(result == T::zero());
        self.write_ea::<T>(instr, instr.get_op2(), result)?;
        advance_cycles!(self, m68000: 0, others: 2)?;
        account_040_time!(
            self,
            if std::mem::size_of::<T>() == 2 {
                timing040::EXT_W
            } else {
                timing040::EXT_L
            },
        );
        Ok(())
    }

    /// SBCD
    fn op_sbcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_x::<Byte>(instr, Self::alu_sub_bcd)?;
        let dn = instr.get_addr_mode_x()? == AddressingMode::DataRegister;
        advance_cycles!(
            self,
            m68000: if dn { 2 } else { 0 },
            others: if dn { 2 } else { 4 },
        )?;
        account_040_time!(
            self,
            if dn {
                timing040::BCD_DN
            } else {
                timing040::BCD_PREDEC
            },
        );

        Ok(())
    }

    /// PACK: adds the adjustment to the two bytes of unpacked BCD of the
    /// source and packs their low nibbles into the destination byte. In
    /// memory, the source bytes are read from -(Ax), the one at the lower
    /// address being the high one (M68000PRM PACK).
    fn op_pack(&mut self, instr: &Instruction) -> Result<()> {
        let adjustment = self.fetch_pump()?;
        let (rx, ry) = (instr.get_op2(), instr.get_op1());
        let packed = |v: Word| (((v >> 4) & 0xF0) | (v & 0x0F)) as Byte;

        if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
            let v = self.regs.read_d::<Word>(rx).wrapping_add(adjustment);
            self.regs.write_d(ry, packed(v));
            // MC68020UM 8.2: 6 clocks in the cache case, 2 for each of the 2
            // instruction words and 2 internal
            advance_cycles!(self, all: 2)?;
        } else {
            let addr = self.regs.read_a_predec::<Address>(rx, 1);
            let low = self.read_ticks::<Byte>(addr)?;
            let addr = self.regs.read_a_predec::<Address>(rx, 1);
            let high = self.read_ticks::<Byte>(addr)?;
            let v = Word::from_be_bytes([high, low]).wrapping_add(adjustment);
            let addr = self.regs.read_a_predec::<Address>(ry, 1);
            // 13 clocks in the cache case: 2 for each of the 2 instruction
            // words and 3 for each of the 3 bus cycles, the 2 byte reads and
            // the write, with no internal time left
            self.write_ticks(addr, packed(v))?;
        }
        account_040_time!(
            self,
            if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
                timing040::PACK_DN
            } else {
                timing040::PACK_PREDEC
            },
        );
        Ok(())
    }

    /// UNPK: unpacks the two BCD digits of the source byte into the low
    /// nibbles of two bytes and adds the adjustment. In memory, the two bytes
    /// are written to -(Ay), the high one at the lower address (M68000PRM
    /// UNPK).
    fn op_unpk(&mut self, instr: &Instruction) -> Result<()> {
        let adjustment = self.fetch_pump()?;
        let (rx, ry) = (instr.get_op2(), instr.get_op1());
        let unpacked = |v: Byte| {
            let v = Word::from(v);
            (((v & 0xF0) << 4) | (v & 0x0F)).wrapping_add(adjustment)
        };

        if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
            let v = unpacked(self.regs.read_d::<Byte>(rx));
            self.regs.write_d(ry, v);
            // MC68020UM 8.2: 8 clocks in the cache case, 2 for each of the 2
            // instruction words and 4 internal
            advance_cycles!(self, all: 4)?;
        } else {
            let addr = self.regs.read_a_predec::<Address>(rx, 1);
            let [high, low] = unpacked(self.read_ticks::<Byte>(addr)?).to_be_bytes();
            // 13 clocks in the cache case: 2 for each of the 2 instruction
            // words and 3 for each of the 3 bus cycles, the read and the 2
            // byte writes, with no internal time left
            let addr = self.regs.read_a_predec::<Address>(ry, 1);
            self.write_ticks(addr, low)?;
            let addr = self.regs.read_a_predec::<Address>(ry, 1);
            self.write_ticks(addr, high)?;
        }
        account_040_time!(
            self,
            if instr.get_addr_mode_x()? == AddressingMode::DataRegister {
                timing040::UNPK_DN
            } else {
                timing040::UNPK_PREDEC
            },
        );
        Ok(())
    }

    /// ABCD
    fn op_abcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_x::<Byte>(instr, Self::alu_add_bcd)?;
        let dn = instr.get_addr_mode_x()? == AddressingMode::DataRegister;
        advance_cycles!(
            self,
            m68000: if dn { 2 } else { 0 },
            others: if dn { 2 } else { 4 },
        )?;
        account_040_time!(
            self,
            if dn {
                timing040::BCD_DN
            } else {
                timing040::BCD_PREDEC
            },
        );

        Ok(())
    }

    /// NBCD
    fn op_nbcd(&mut self, instr: &Instruction) -> Result<()> {
        self.op_alu_zero::<Byte>(instr, Self::alu_sub_bcd)?;
        if instr.get_addr_mode()? == AddressingMode::DataRegister {
            advance_cycles!(self, m68000: 2, others: 4)?;
        }

        account_040_time!(self, timing040::NBCD, instr.get_addr_mode()?);
        Ok(())
    }

    /// LEA/PEA
    fn op_lea_pea(&mut self, instr: &Instruction) -> Result<()> {
        let value: Long =
            self.calc_ea_addr::<Long>(instr, instr.get_addr_mode()?, instr.get_op2())?;

        // On the 68020/68030 the calculate EA tables (MC68020UM 8.2.3, MC68030UM
        // 11.6.3) give 1 cycle more than the fetch EA times for (An), none for the
        // brief format and 2 for the full format.
        match instr.get_addr_mode()? {
            AddressingMode::IndirectIndex | AddressingMode::PCIndex => {
                advance_cycles!(
                    self,
                    m68000: 2,
                    others: if instr.get_extword().is_full() { 2 } else { 0 },
                )?;
                self.prefetch_pump()?;
            }
            AddressingMode::AbsoluteShort | AddressingMode::AbsoluteLong => (),
            AddressingMode::Indirect => {
                advance_cycles!(self, m68000: 0, others: 1)?;
                self.prefetch_pump()?;
            }
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
        account_040_time!(
            self,
            if instr.mnemonic == InstructionMnemonic::PEA {
                timing040::PEA
            } else {
                timing040::LEA
            },
            instr.get_addr_mode()?,
        );
        Ok(())
    }

    /// TAS
    pub fn op_tas(&mut self, instr: &Instruction) -> Result<()> {
        // 68k reserved illegal opcodes (invalid modes for TAS)
        if [
            AddressingMode::AddressRegister,
            AddressingMode::Immediate,
            AddressingMode::PCIndex,
            AddressingMode::PCDisplacement,
        ]
        .contains(&instr.get_addr_mode()?)
        {
            return self.raise_illegal_instruction();
        }

        let v = self.read_ea::<Byte>(instr, instr.get_op2())?;
        if instr.get_addr_mode()? != AddressingMode::DataRegister {
            advance_cycles!(self, m68000: 2, m68020: 5, m68030: 7)?;
        } else {
            advance_cycles!(self, m68000: 0, others: 2)?;
        }
        self.write_ea(instr, instr.get_op2(), v | 0x80)?;
        self.regs.sr.set_z(v == 0);
        self.regs.sr.set_n(v & 0x80 != 0);
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);
        account_040_time!(self, timing040::TAS, instr.get_addr_mode()?);
        Ok(())
    }

    /// TST
    fn op_tst<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let result: T = self.read_ea(instr, instr.get_op2())?;
        self.regs.sr.set_z(result == T::zero());
        self.regs.sr.set_n(result & T::msb() != T::zero());
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);
        account_040_time!(self, timing040::ALU, instr.get_addr_mode()?);
        Ok(())
    }

    /// What LINK pushes for the address register `reg`, given the decremented
    /// stack pointer. A 68LC040 was seen to push the decremented value for LINK
    /// A7, in the order of the manual's operation (SP - 4 -> SP, An -> (SP)). Up to
    /// the 68030 this keeps the old value, as in the 68000 data from MAME's microcode.
    fn link_value(&self, reg: usize, sp: Address) -> Address {
        if CPU_TYPE >= M68040 && reg == 7 {
            sp
        } else {
            self.regs.read_a(reg)
        }
    }

    /// LINK.w
    fn op_link_w(&mut self, instr: &Instruction) -> Result<()> {
        let sp = self.regs.read_a::<Address>(7).wrapping_sub(4);
        let addr = self.link_value(instr.get_op2(), sp);

        instr.fetch_extword(|| self.fetch_pump())?;

        self.write_ticks(sp, addr)?;
        self.regs.write_a(instr.get_op2(), sp);
        self.regs.read_a_predec::<Address>(7, 4);
        self.regs
            .write_a(7, sp.wrapping_add_signed(instr.get_displacement()));

        account_040_time!(self, timing040::LINK);
        Ok(())
    }

    /// LINK.l
    fn op_link_l(&mut self, instr: &Instruction) -> Result<()> {
        let sp = self.regs.read_a::<Address>(7).wrapping_sub(4);
        let addr = self.link_value(instr.get_op2(), sp);

        let displacement = {
            let msb = self.fetch_pump()? as Long;
            let lsb = self.fetch_pump()? as Long;
            ((msb << 16) | lsb) as i32
        };

        self.write_ticks(sp, addr)?;
        self.regs.write_a(instr.get_op2(), sp);
        self.regs.read_a_predec::<Address>(7, 4);
        self.regs.write_a(7, sp.wrapping_add_signed(displacement));

        account_040_time!(self, timing040::LINK);
        Ok(())
    }

    /// UNLINK
    fn op_unlink(&mut self, instr: &Instruction) -> Result<()> {
        let addr = self.regs.read_a::<Address>(instr.get_op2());
        let val = self.read_ticks::<Address>(addr)?;
        self.regs.write_a(7, addr.wrapping_add(4));
        self.regs.write_a(instr.get_op2(), val);
        advance_cycles!(self, m68000: 0, others: 1)?;
        account_040_time!(self, timing040::UNLK);
        Ok(())
    }

    /// RESET
    fn op_reset(&mut self, _instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        debug!("RESET instruction");
        // 518 clocks on the 68020/68030 (MC68020UM 8.2.17, MC68030UM 11.6.17)
        advance_cycles!(self, m68000: 128, others: 516)?;
        account_040_time!(self, timing040::RESET);

        // The MacII / System 4.2 restart routine relies on part of a JMP
        // being in the prefetch queue because pulling on reset will re-
        // activate overlay. Re-fill now to have the full JMP pre-fetched.
        self.prefetch_refill()?;

        // Pull on reset
        let halt_asserted = self.bus.reset(false)?;

        if halt_asserted {
            // HALT was asserted, so hard-reset the CPU as well.
            self.reset()?;
        } else if CPU_TYPE == M68020 {
            // The (external) FPU and PMMU are connected to the RESET line,
            // so we reset them here.
            // Not for the models with a CPU with a built-in FPU.
            self.regs.fpu = FpuRegisterFile::default();
            self.regs.pmmu = PmmuRegisterFile::default();
        }

        Ok(())
    }

    /// STOP - Load immediate data into SR and wait for interrupt
    fn op_stop(&mut self, _instr: &Instruction) -> Result<()> {
        let new_sr = self.fetch()?;

        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        self.prefetch_pump()?;
        self.set_sr(new_sr);
        advance_cycles!(self, m68000: 0, others: 4)?;

        // TODO STOP ignored for now
        // Enables keyboard input in A/UX
        Ok(())
    }

    /// RTE
    /// RTE reads the rest of a bus fault frame of the 68020/68030: `longs`
    /// long words after the format word. The manuals count 10 reads for the
    /// short frame, 24 (68020) or 25 (68030) for the long one, the SR, PC
    /// and format word reads included (MC68020UM 8.2.18, MC68030UM 11.6.18).
    fn rte_fault_frame(&mut self, longs: Address) -> Result<()> {
        let ssp = *self.regs.ssp();
        for i in 0..longs {
            self.read_ticks::<Long>(ssp.wrapping_add(8 + 4 * i))?;
        }
        Ok(())
    }

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

        // 68010+ version
        let ssp = *self.regs.ssp();
        let format = (self.read_ticks::<Word>(ssp.wrapping_add(6))? & 0b1111_0000_0000_0000) >> 12;
        match format {
            0b0000 => {
                // Normal format
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(8);
                advance_cycles!(self, m68000: 0, m68020: 5, m68030: 2)?;
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b0001 if CPU_TYPE >= M68020 => {
                // Throwaway frame: its SR selects the stack holding the frame
                // to return through (M68030UM 8.4, M68040UM figure 8-6)
                let sr = self.read_ticks::<Word>(ssp.wrapping_add(0))?;
                *self.regs.ssp_mut() = ssp.wrapping_add(8);
                self.set_sr(sr);
                self.op_rte(_instr)?;
                account_040_time!(self, timing040::rte(format));
                return Ok(());
            }
            0b0010 if CPU_TYPE >= M68020 => {
                // Six word frame
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(12);
                advance_cycles!(self, m68000: 0, m68020: 5, m68030: 2)?;
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b1001 if (M68020..=M68030).contains(&CPU_TYPE) => {
                // Coprocessor mid-instruction: the CPU reads the whole frame and the
                // FPU instruction starts again from its address
                let ssp = *self.regs.ssp();
                let sr = self.read_ticks::<Word>(ssp)?;
                self.read_ticks::<Long>(ssp.wrapping_add(2))?;
                let pc = self.read_ticks(ssp.wrapping_add(8))?;
                self.read_ticks::<Long>(ssp.wrapping_add(0x0C))?;
                self.read_ticks::<Long>(ssp.wrapping_add(0x10))?;
                *self.regs.ssp_mut() = ssp.wrapping_add(20);
                advance_cycles!(self, m68000: 0, m68020: 7, m68030: 8)?;
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b1010 if (M68020..=M68030).contains(&CPU_TYPE) => {
                // Bus error at instruction boundary
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                self.rte_fault_frame(6)?;
                advance_cycles!(self, m68000: 0, m68020: 9, m68030: 2)?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(32);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b1011 if (M68020..=M68030).contains(&CPU_TYPE) => {
                // Bus error during instruction
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                self.rte_fault_frame(if CPU_TYPE == M68030 { 21 } else { 20 })?;
                // The 68030 takes 3 clocks more than the manual: our reads take 3 clocks
                advance_cycles!(self, m68020: 16)?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(92);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b0111
                if CPU_TYPE >= M68040 && Ssw040(self.read_ticks(ssp.wrapping_add(0x0C))?).ct() =>
            {
                // M68040 access error with a pending trace: the frame becomes
                // the format $2 frame of the trace exception (M68040UM 8.4.6.2)
                let sr = self.read_ticks::<Word>(ssp.wrapping_add(0))?;
                let pc = self.read_ticks::<Long>(ssp.wrapping_add(2))?;
                let ea = self.read_ticks::<Long>(ssp.wrapping_add(8))?;
                let ssp = ssp.wrapping_add(0x30);
                *self.regs.ssp_mut() = ssp;
                self.write_ticks(ssp.wrapping_add(0), sr)?;
                self.write_ticks(ssp.wrapping_add(2), pc)?;
                self.write_ticks(
                    ssp.wrapping_add(6),
                    0b0010_0000_0000_0000 | VECTOR_TRACE as u16,
                )?;
                self.write_ticks(ssp.wrapping_add(8), ea)?;
                let new_pc = self.read_ticks::<Long>(self.regs.vbr.wrapping_add(VECTOR_TRACE))?;
                self.set_pc(new_pc)?;
            }
            0b0011 if CPU_TYPE >= M68040 => {
                // M68040 floating-point post-instruction, six words (M68040UM 8.4.4)
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(12);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b0111 if CPU_TYPE >= M68040 => {
                // M68040 access error
                let sr = self.read_ticks::<Word>(self.regs.ssp().wrapping_add(0))?;
                let pc = self.read_ticks(self.regs.ssp().wrapping_add(2))?;
                *self.regs.ssp_mut() = self.regs.ssp().wrapping_add(0x3C);
                self.set_sr(sr);
                self.set_pc(pc)?;
            }
            0b1001 if (M68020..=M68030).contains(&CPU_TYPE) => {
                bail!("Coprocessor mid-instruction frame not supported")
            }
            0b1000 if CPU_TYPE == M68010 => bail!("68010 bus error frame not supported"),
            // Not a frame of this CPU
            _ => return self.raise_exception(ExceptionGroup::Group2, VECTOR_FORMAT_ERROR, None),
        }

        self.prefetch_refill()?;
        self.test_step_out();

        account_040_time!(self, timing040::rte(format));
        Ok(())
    }

    /// RTS
    fn op_rts(&mut self, _instr: &Instruction) -> Result<()> {
        let pc = self.read_ticks(self.regs.read_a(7))?;
        self.regs.read_a_postinc::<Address>(7, 4);
        advance_cycles!(self, m68000: 0, others: 3)?;
        account_040_time!(self, timing040::RTS);
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
        advance_cycles!(self, m68000: 0, others: 4)?;
        account_040_time!(self, timing040::RTR);
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
                advance_cycles!(
                    self,
                    m68000: 2,
                    others: if instr.mnemonic == InstructionMnemonic::JMP { 2 } else { 0 },
                )?;
                self.regs.pc = self.regs.pc.wrapping_add(2) & ADDRESS_MASK;
                self.fetch()?.expand_signed().cast_unsigned()
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

        // Idle cycles. On the 68020/68030 these come from the jump EA tables
        // (MC68020UM 8.2.5, MC68030UM 11.6.5) less the fetch EA times. JSR takes 2
        // cycles less than JMP there, as the stacking overlaps. The totals of JMP
        // and JSR are still a few clocks off the manuals.
        advance_cycles!(
            self,
            m68000: match instr.get_addr_mode()? {
                AddressingMode::IndirectDisplacement | AddressingMode::PCDisplacement => 2,
                AddressingMode::IndirectIndex | AddressingMode::PCIndex => 4,
                _ => 0,
            },
            others: if instr.mnemonic == InstructionMnemonic::JMP {
                match instr.get_addr_mode()? {
                    AddressingMode::Indirect => 1,
                    AddressingMode::IndirectDisplacement
                    | AddressingMode::PCDisplacement
                    | AddressingMode::IndirectIndex
                    | AddressingMode::PCIndex => 2,
                    _ => 0,
                }
            } else {
                0
            },
        )?;
        account_040_time!(
            self,
            if instr.mnemonic == InstructionMnemonic::JSR {
                timing040::JSR
            } else {
                timing040::JMP
            },
            instr.get_addr_mode()?,
        );

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
            self.regs.write(reg, v.expand_signed().cast_unsigned());

            if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
                addr = addr.wrapping_add(std::mem::size_of::<T>() as Address);
            }
        }

        if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
            addr = addr.wrapping_sub(std::mem::size_of::<T>() as Address);
        }
        let n = Ticks::from(mask.count_ones());
        advance_cycles!(self, m68000: 0, others: 5 + n)?;

        // Discarded read
        if CPU_TYPE == M68000 {
            self.read_ticks::<Word>(addr)?;
        }

        // Update the EA An register with the final address on predec/postinc
        match instr.get_addr_mode()? {
            AddressingMode::IndirectPostInc | AddressingMode::IndirectPreDec => {
                self.regs.write_a(instr.get_op2(), addr);
            }
            _ => (),
        }

        account_040_time!(
            self,
            self.exec040_movem(instr, mask, true, std::mem::size_of::<T>() == 2)?,
        );
        Ok(())
    }

    /// MOVEM register to memory
    fn op_movem_mem<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let mask = self.fetch_pump()?;
        let mut addr = self.calc_ea_addr_no_mod::<T>(instr, instr.get_op2())?;
        // MC68020UM 8.2.7: 4 + 3n clocks, MC68030UM 11.6.7: 4 + 2n, with
        // n writes. Our writes take 3 clocks: the 68030 runs n clocks long.
        advance_cycles!(self, m68000: 0, others: 4)?;

        let regs = if instr.get_addr_mode()? != AddressingMode::IndirectPreDec {
            Either::Left(MOVEM_REGS.iter().rev())
        } else {
            Either::Right(MOVEM_REGS.iter())
        };

        // The 68020+ store the addressing register decremented by the size of
        // one operation, the 68000 as it was (M68000PRM MOVEM)
        let predec_an = (CPU_TYPE >= M68020
            && instr.get_addr_mode()? == AddressingMode::IndirectPreDec)
            .then(|| Register::An(instr.get_op2()));
        let an_value = T::chop(addr.wrapping_sub(std::mem::size_of::<T>() as Address));

        for (_, &reg) in regs.enumerate().filter(|(i, _)| mask & (1 << i) != 0) {
            let v = if predec_an == Some(reg) {
                an_value
            } else {
                self.regs.read::<T>(reg)
            };
            if instr.get_addr_mode()? == AddressingMode::IndirectPreDec {
                addr = addr.wrapping_sub(std::mem::size_of::<T>() as Address);
                if CPU_TYPE == M68000 {
                    self.write_ticks_wflip(addr, v)?;
                } else {
                    self.write_ticks(addr, v)?;
                }
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

        account_040_time!(
            self,
            self.exec040_movem(instr, mask, false, std::mem::size_of::<T>() == 2)?,
        );
        Ok(())
    }

    /// CHK
    fn op_chk<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        let max = self.read_ea::<T>(instr, instr.get_op2())?.expand_signed();
        let value = self.regs.read_d::<T>(instr.get_op1()).expand_signed();

        let (_result, ccr) =
            Self::alu_sub::<T>(T::chop(max as u32), T::chop(value as u32), self.regs.sr);
        let t = RegisterSR::default().with_ccr(ccr);

        let indirect = instr.get_addr_mode()? == AddressingMode::Indirect;
        advance_cycles!(self, all: if indirect { 2 } else { 0 })?;

        self.regs.sr.set_n(value < 0);
        self.regs.sr.set_z(value == 0);
        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);

        if t.v() || t.n() {
            // Short trap. Only the 68030 manual has a time for the trap; the 68020
            // takes the same here.
            advance_cycles!(
                self,
                m68000: if indirect { 6 } else { 8 },
                others: if indirect { 12 } else { 14 },
            )?;
            // Offset PC correctly for exception stack frame for CHK
            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_CHK, None);
        } else if self.regs.sr.n() {
            // Long trap
            advance_cycles!(
                self,
                m68000: if indirect { 8 } else { 10 },
                others: if indirect { 12 } else { 14 },
            )?;
            // Offset PC correctly for exception stack frame for CHK
            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_CHK, None);
        }

        advance_cycles!(self, all: if indirect { 4 } else { 6 })?;
        account_040_time!(self, timing040::CHK, instr.get_addr_mode()?);
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
        if CPU_TYPE == M68000 {
            // Discarded read
            self.read_ea::<Byte>(instr, instr.get_op2())?;
            self.prefetch_pump()?;
        }

        let result = if self.cc(instr.get_cc()) { 0xFF } else { 0 };

        // Idle cycles
        let dn = instr.get_addr_mode()? == AddressingMode::DataRegister;
        advance_cycles!(
            self,
            m68000: if dn && result != 0 { 2 } else { 0 },
            others: if dn { 2 } else { 1 },
        )?;

        self.write_ea::<Byte>(instr, instr.get_op2(), result)?;
        account_040_time!(self, timing040::SCC, instr.get_addr_mode()?);
        Ok(())
    }

    /// DBcc
    fn op_dbcc(&mut self, instr: &Instruction) -> Result<()> {
        instr.fetch_extword(|| self.fetch())?;
        let displacement = instr.get_displacement();

        advance_cycles!(self, all: 2)?; // idle

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
                account_040_time!(self, timing040::DBCC_TAKEN);
            } else {
                // Loop terminated
                self.history_current.branch_taken = Some(false);
                advance_cycles!(self, all: 4)?; // idle
                account_040_time!(self, timing040::DBCC_NOT_TAKEN);
            }
        } else {
            self.history_current.branch_taken = Some(false);
            advance_cycles!(self, m68000: 2)?; // idle
            account_040_time!(self, timing040::DBCC_NOT_TAKEN);
        }

        Ok(())
    }

    /// Bcc/BSR
    fn op_bcc<const BSR: bool>(&mut self, instr: &Instruction) -> Result<()> {
        let long = CPU_TYPE >= M68020 && instr.get_bxx_displacement_raw() == 0xFF;
        let displacement = if instr.get_bxx_displacement() == 0 {
            instr.fetch_extword(|| self.fetch())?;
            instr.get_displacement()
        } else if long {
            let msb = self.fetch_pump()? as Address;
            let lsb = self.fetch_pump()? as Address;
            // -4 since we just nudged the PC twice
            ((msb << 16) | lsb) as i32 - 4
        } else {
            instr.get_bxx_displacement()
        };

        // On the 68020/68030, BSR and the long forms have no idle cycles. The
        // totals of the word and long forms are still a few clocks off the
        // manuals, as their extension words and the refill take that long already.
        advance_cycles!(
            self,
            m68000: 2,
            others: if BSR || long { 0 } else { 2 },
        )?; // idle

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
            account_040_time!(
                self,
                if BSR {
                    timing040::BSR
                } else {
                    timing040::BCC_TAKEN
                },
            );
        } else {
            // Branch not taken
            self.history_current.branch_taken = Some(false);

            advance_cycles!(self, m68000: 2)?; // idle
            account_040_time!(self, timing040::BCC_NOT_TAKEN);
        }
        Ok(())
    }

    /// MOVEQ
    fn op_moveq(&mut self, instr: &Instruction) -> Result<()> {
        let value: Long = (instr.data as u8).expand_signed().cast_unsigned();

        self.regs.write_d(instr.get_op1(), value);

        self.regs.sr.set_c(false);
        self.regs.sr.set_v(false);
        self.regs.sr.set_z(value == 0);
        self.regs.sr.set_n(value & Long::msb() != 0);

        account_040_time!(self, timing040::MOVEQ);
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
        advance_cycles!(self, m68020: 0, others: 2)?; // idle
        account_040_time!(
            self,
            if matches!(reg_l, Register::An(_)) {
                timing040::EXG_AN
            } else {
                timing040::EXG
            },
        );

        Ok(())
    }

    /// ASd, LSd, ROd, ROXd
    fn op_shrot<T: CpuSized>(
        &mut self,
        instr: &Instruction,
        calcfn: fn(T, u8, RegisterSR) -> (T, u8),
    ) -> Result<()> {
        let count = match instr.get_sh_count() {
            Either::Left(i) => i,
            Either::Right(r) => (self.regs.read::<Long>(r) % 64) as u8,
        };

        self.prefetch_pump()?;

        let value = self.regs.read_d::<T>(instr.get_op2());
        let (result, ccr) = calcfn(value, count, self.regs.sr);
        self.regs.write_d(instr.get_op2(), result);
        self.regs.sr.set_ccr(ccr);

        // The 68000 takes 2 clocks per bit, the 68020/68030 barrel shifter a
        // fixed time whatever the count
        let dynamic = instr.get_sh_count().is_right();
        let count_over_size = count as usize > std::mem::size_of::<T>() * 8;
        advance_cycles!(
            self,
            m68000: 2 * count as Ticks + if std::mem::size_of::<T>() == 4 { 4 } else { 2 },
            m68020: Self::shrot_idle_020(instr, dynamic),
            m68030: Self::shrot_idle_030(instr, dynamic, count_over_size),
        )?;
        account_040_time!(
            self,
            self.exec040_ea(
                timing040::shift_column(instr.mnemonic),
                AddressingMode::DataRegister
            )
            .plus(u16::from(dynamic)),
        );

        Ok(())
    }

    /// Idle clocks of a register shift or rotate on the 68020 (MC68020UM 8.2.12)
    fn shrot_idle_020(instr: &Instruction, dynamic: bool) -> Ticks {
        use InstructionMnemonic::*;
        match instr.mnemonic {
            LSL_b | LSL_w | LSL_l | LSR_b | LSR_w | LSR_l => {
                if dynamic {
                    4
                } else {
                    2
                }
            }
            ASL_b | ASL_w | ASL_l => 6,
            ASR_b | ASR_w | ASR_l => 4,
            ROL_b | ROL_w | ROL_l | ROR_b | ROR_w | ROR_l => 6,
            ROXL_b | ROXL_w | ROXL_l | ROXR_b | ROXR_w | ROXR_l => 10,
            _ => unreachable!(),
        }
    }

    /// Idle clocks of a register shift or rotate on the 68030 (MC68030UM 11.6.12),
    /// which is slower when a count in a register is larger than the operand size
    fn shrot_idle_030(instr: &Instruction, dynamic: bool, count_over_size: bool) -> Ticks {
        use InstructionMnemonic::*;
        match instr.mnemonic {
            LSL_b | LSL_w | LSL_l | LSR_b | LSR_w | LSR_l => match (dynamic, count_over_size) {
                (false, _) => 2,
                (true, false) => 4,
                (true, true) => 6,
            },
            ASL_b | ASL_w | ASL_l => {
                if dynamic {
                    6
                } else {
                    4
                }
            }
            ASR_b | ASR_w | ASR_l => match (dynamic, count_over_size) {
                (false, _) => 2,
                (true, false) => 4,
                (true, true) => 8,
            },
            ROL_b | ROL_w | ROL_l | ROR_b | ROR_w | ROR_l => {
                if dynamic {
                    6
                } else {
                    4
                }
            }
            ROXL_b | ROXL_w | ROXL_l | ROXR_b | ROXR_w | ROXR_l => 10,
            _ => unreachable!(),
        }
    }

    /// ASd, LSd, ROd, ROXd (effective address, always Word)
    fn op_shrot_ea(
        &mut self,
        instr: &Instruction,
        calcfn: fn(Word, u8, RegisterSR) -> (Word, u8),
    ) -> Result<()> {
        let value = self.read_ea::<Word>(instr, instr.get_op2())?;

        self.prefetch_pump()?;

        let (result, ccr) = calcfn(value, 1, self.regs.sr);
        self.write_ea::<Word>(instr, instr.get_op2(), result)?;
        self.regs.sr.set_ccr(ccr);

        // Idle cycles: ASL and the rotates take longer than the other shifts
        let (asl, rotate) = (
            instr.mnemonic == InstructionMnemonic::ASL_ea,
            matches!(
                instr.mnemonic,
                InstructionMnemonic::ROL_ea | InstructionMnemonic::ROR_ea
            ),
        );
        advance_cycles!(
            self,
            m68000: 0,
            m68020: if asl { 1 } else if rotate { 2 } else { 0 },
            m68030: if asl || rotate { 2 } else { 0 },
        )?;
        account_040_time!(
            self,
            *timing040::shift_column(instr.mnemonic),
            instr.get_addr_mode()?,
        );

        Ok(())
    }

    /// Transfers one 16-byte line from one physical address to another (MOVE16)
    fn move16_transfer(&mut self, src: Address, dest: Address) -> Result<()> {
        // The low four address bits are ignored: the transfer is always of an
        // aligned 16-byte line. The whole line is read before any of it is
        // written.
        let line = self
            .line_ticks(src, None)
            .map_err(|e| Self::move16_fault(e, src))?;
        self.line_ticks(dest, Some(&line))
            .map_err(|e| Self::move16_fault(e, dest))?;
        Ok(())
    }

    /// A MOVE16 access faults as a whole line, at the address the
    /// instruction gave (M68040UM 8.4.6.4)
    fn move16_fault(e: anyhow::Error, addr: Address) -> anyhow::Error {
        match e.downcast_ref() {
            Some(CpuError::BusError(details)) => anyhow!(CpuError::BusError(Group0Details {
                size: 16,
                address: addr,
                ..*details
            })),
            _ => e,
        }
    }

    /// MOVE16 (Ax)+,(Ay)+
    fn op_move16_inc(&mut self, instr: &Instruction) -> Result<()> {
        instr.fetch_extword(|| self.fetch())?;

        let ax = instr.get_op2();
        let ay = usize::from((u16::from(instr.get_extword()) >> 12) & 0b111);
        let src = self.regs.read_a::<Address>(ax);
        let dest = self.regs.read_a::<Address>(ay);

        self.prefetch_pump()?;
        self.move16_transfer(src, dest)?;

        // Both registers are incremented by 16, but if they are the same
        // register it is only incremented once.
        self.regs.write_a(ax, src.wrapping_add(16));
        if ax != ay {
            self.regs.write_a(ay, dest.wrapping_add(16));
        }

        account_040_time!(self, timing040::MOVE16_INC);
        Ok(())
    }

    /// MOVE16 (absolute modes)
    fn op_move16_abs(&mut self, instr: &Instruction) -> Result<()> {
        let to_absolute = instr.data & (1 << 3) == 0;
        let postinc = instr.data & (1 << 4) == 0;

        let an = instr.get_op2();
        let anaddr = self.regs.read_a::<Address>(an);
        let absaddr = ((self.fetch_pump()? as Address) << 16) | self.fetch_pump()? as Address;

        self.prefetch_pump()?;
        if to_absolute {
            self.move16_transfer(anaddr, absaddr)?;
        } else {
            self.move16_transfer(absaddr, anaddr)?;
        }

        if postinc {
            self.regs.write_a(an, anaddr.wrapping_add(16));
        }

        account_040_time!(
            self,
            if !to_absolute && postinc {
                timing040::MOVE16_TO_AN_INC
            } else {
                timing040::MOVE16_ABS
            },
        );
        Ok(())
    }

    /// CINV/CPUSH
    fn op_cxxx(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        // Cache field: 01 = data, 10 = instruction, 11 = both
        let caches = match (instr.data >> 6) & 0b11 {
            0b01 => CacheSelect::Data,
            0b10 => CacheSelect::Instruction,
            0b11 => CacheSelect::Both,
            _ => return Ok(()),
        };
        let scope = match (instr.data >> 3) & 0b11 {
            0b01 => CacheScope::Line,
            0b10 => CacheScope::Page(1 << self.mmu040_page_shift()),
            0b11 => CacheScope::All,
            _ => return Ok(()),
        };
        let push = instr.data & (1 << 5) != 0;
        let paddr = self.regs.read_a::<Address>(instr.get_op2());
        account_040_time!(self, timing040::cinv_cpush(push, (instr.data >> 3) & 0b11));
        let mut ticks = 0;
        self.caches.invalidate(
            &mut bus_transaction!(self, &mut ticks),
            caches,
            scope,
            paddr,
            push,
        )?;
        self.advance_cycles(ticks)
    }

    /// PFLUSH/PFLUSHN/PFLUSHA/PFLUSHAN (M68040)
    fn op_pflush040(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        // Just conservatively flush the entire ATC
        // TODO targetted flushing (I dont think it matters on the Mac)
        self.pmmu_cache_invalidate();
        // Opmode 10 is PFLUSHAN
        account_040_time!(
            self,
            if instr.data & 0b11000 == 0b10000 {
                timing040::PFLUSHAN
            } else {
                timing040::PFLUSH
            },
        );
        Ok(())
    }

    /// PTESTR/PTESTW (M68040)
    fn op_ptest040(&mut self, instr: &Instruction) -> Result<()> {
        if !self.regs.sr.supervisor() {
            return self.raise_privilege_violation();
        }

        // The address space to search comes from DFC, the address from An.
        let fc = (self.regs.dfc & 0b111) as u8;
        let vaddr = self.regs.read_a::<Address>(instr.get_op2());
        account_040_time!(self, timing040::PTEST);
        self.mmu040_ptest(fc, vaddr)
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

        let ctrlreg = match instr.movec_ctrlreg() {
            Ok(reg) if reg.exists_on(CPU_TYPE) => reg,
            _ => return self.raise_illegal_instruction(),
        };

        if instr.movec_ctrl_to_gen() {
            let val = self.regs.read::<Long>(ctrlreg.into());
            self.regs.write(instr.movec_reg(), val);
            advance_cycles!(self, all: 2)?;
        } else {
            let val = self.regs.read::<Long>(instr.movec_reg());
            let destreg: Register = ctrlreg.into();

            if destreg == Register::CACR {
                // Cache operations
                let mask = match CPU_TYPE {
                    M68020 => M68020_CACR_MASK,
                    M68030 => M68030_CACR_MASK,
                    M68040 => M68040_CACR_MASK,
                    _ => unreachable!(),
                };
                // The caches run the cache operations and say what stays
                let val = self.caches.write_cacr(val, self.regs.caar);
                self.regs.write(destreg, val & mask);
            } else {
                self.regs.write(destreg, val);
                self.pmmu_translation_changed();

                if CPU_TYPE >= M68040
                    && matches!(
                        destreg,
                        Register::TC040 | Register::SRP040 | Register::URP040
                    )
                {
                    self.pmmu_cache_invalidate();
                    self.pmmu_cache_ensure();
                }
            }
            // The 68030 is faster for the control registers other than SFC, DFC
            // and CACR (MC68030UM 11.6.7)
            match instr.movec_ctrlreg()? {
                MovecCtrlReg::SFC | MovecCtrlReg::DFC | MovecCtrlReg::CACR => {
                    advance_cycles!(self, all: 8)?;
                }
                _ => advance_cycles!(self, m68000: 0, m68020: 8, m68030: 2)?,
            }
        }

        account_040_time!(
            self,
            if instr.movec_ctrl_to_gen() {
                timing040::MOVEC_FROM_CTRL
            } else {
                timing040::MOVEC_TO_CTRL
            },
        );
        Ok(())
    }

    /// Tests for an access to the coprocessor interface registers of the FPU,
    /// which ends in a bus error if there is no FPU.
    fn is_fpu_cir_access<T: CpuSized>(&self, fc: u8, addr: Address, read: bool) -> Result<bool> {
        if fc != FC_CPU_SPACE || addr & !FPU_CIR_MASK != FPU_CIR_BASE {
            return Ok(false);
        }
        if FPU_TYPE == FPU_NONE {
            bail!(CpuError::BusError(Group0Details {
                function_code: fc,
                ir: 0,
                size: std::mem::size_of::<T>(),
                instruction: false,
                read,
                address: addr,
                atc_fault: false,
                // Filled in later
                start_pc: 0,
            }));
        }
        Ok(true)
    }

    /// MOVES - Move data to/from address space
    fn op_moves<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        // Fetch/decode the extension word
        // Leave instr.extword clean for EA calculation
        let ext_word = self.fetch()?;
        let reg_num = (ext_word >> 12) & 0b111;
        let is_addr_reg = (ext_word & 0x8000) != 0;
        let dir_mem_to_reg = (ext_word & 0x0800) == 0;

        if !self.regs.sr.supervisor() {
            advance_cycles!(self, m68000: 0, m68020: 1, m68030: 4)?;
            account_040_time!(self, timing040::TRAP);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_PRIVILEGE_VIOLATION, None);
        }

        let ea = self.calc_ea_addr::<T>(instr, instr.get_addr_mode()?, instr.get_op2())?;

        if dir_mem_to_reg {
            // Memory to register transfer
            let value = if self.is_fpu_cir_access::<T>(self.regs.sfc as u8, ea, true)? {
                // Only the response register of an idle FPU is modelled
                let regs = if ea == FPU_CIR_BASE {
                    FPU_CIR_RESPONSE_NULL
                } else {
                    0
                };
                T::chop(regs >> (32 - 8 * std::mem::size_of::<T>()))
            } else {
                self.read_ticks_generic::<T>(AccessKind::Data, self.regs.sfc as u8, ea)?
            };

            if is_addr_reg {
                // Memory to address register - sign extend
                self.regs
                    .write_a(reg_num as usize, value.expand_signed().cast_unsigned());
            } else {
                // Memory to data register
                self.regs.write_d::<T>(reg_num as usize, value);
            }

            advance_cycles!(self, m68030: 1)?;
        } else {
            // Register to memory transfer
            let value = if is_addr_reg {
                T::chop(self.regs.read_a(reg_num as usize))
            } else {
                self.regs.read_d::<T>(reg_num as usize)
            };

            if !self.is_fpu_cir_access::<T>(self.regs.dfc as u8, ea, false)? {
                self.write_ticks_order_generic::<T, TORDER_LOWHIGH>(
                    AccessKind::Data,
                    self.regs.dfc as u8,
                    ea,
                    value,
                )?;
            }
        }

        self.prefetch_pump()?;
        account_040_time!(
            self,
            match (dir_mem_to_reg, is_addr_reg) {
                (true, true) => timing040::MOVES_TO_AN,
                (true, false) => timing040::MOVES_TO_DN,
                (false, _) => timing040::MOVES_TO_MEM,
            },
            instr.get_addr_mode()?,
        );
        Ok(())
    }

    /// RTD
    fn op_rtd(&mut self, _instr: &Instruction) -> Result<()> {
        // Bus access and cycles are an approximation based on UM/PRM
        let displacement = self.fetch()?.expand_signed();
        let pc = self.read_ticks(self.regs.read_a(7))?;
        let sp = self.regs.read_a::<Address>(7);
        self.regs
            .write_a(7, sp.wrapping_add_signed(4 + displacement));
        advance_cycles!(self, m68000: 0, m68020: 3, m68030: 4)?;
        account_040_time!(self, timing040::RTD);
        self.set_pc(pc)?;
        self.prefetch_refill()?;

        self.test_step_out();

        Ok(())
    }

    /// Reads the part of a memory bit field that lies in the long word at
    /// `ea` (`offset` is 0-7), left-aligned in a long word.
    ///
    /// Only the bytes the field spans are accessed. The Motorola manuals
    /// are not explicit about the size of a bit field's operand transfer:
    /// the MC68020/MC68030 timing tables only say a field takes one
    /// operand cycle up to 4 bytes and two for 5, and the MC68030 manual
    /// (3.5.3) warns that a field insert into a peripheral register may
    /// affect other fields of that register. Other evidence agrees on
    /// sizing by span: WinUAE's cycle-exact cores do so, a 68030 was seen
    /// issuing a 3-byte transfer for `bftst (a0){0:24}` at an odd address
    /// (NetBSD amiga-dev, March 1994), and the Apple IIe Card's driver
    /// updates two bits of a card register with BFINS three bytes before
    /// a data port that a long access would disturb.
    fn read_bitfield(&mut self, ea: Address, offset: i32, width: Long) -> Result<Long> {
        let len = (offset as Long + width).div_ceil(8).min(4);
        Ok(self.read_ticks_len(ea, len)? << (8 * (4 - len)))
    }

    /// Writes back the part of a memory bit field that lies in the long
    /// word at `ea`, accessing only the bytes the field spans (see
    /// [`Self::read_bitfield`]).
    fn write_bitfield(&mut self, ea: Address, offset: i32, width: Long, value: Long) -> Result<()> {
        let len = (offset as Long + width).div_ceil(8).min(4);
        self.write_ticks_len(ea, len, value >> (8 * (4 - len)))
    }

    /// Idle clocks of a bit field instruction on the 68020: the table time
    /// (MC68020UM 8.2.14) less the opcode and extension words and the operand bus
    /// cycles. `five_bytes` is None for a data register, otherwise whether the field
    /// in memory spans five bytes.
    fn bf_idle_020(instr: &Instruction, five_bytes: Option<bool>) -> Ticks {
        // Dn, memory < 5 bytes, memory 5 bytes
        let (dn, mem, mem5) = match instr.mnemonic {
            InstructionMnemonic::BFTST => (2, 4, 5),
            InstructionMnemonic::BFCHG
            | InstructionMnemonic::BFCLR
            | InstructionMnemonic::BFSET => (8, 6, 8),
            InstructionMnemonic::BFEXTS | InstructionMnemonic::BFEXTU => (4, 6, 8),
            InstructionMnemonic::BFINS => (6, 4, 4),
            InstructionMnemonic::BFFFO => (14, 17, 22),
            _ => unreachable!(),
        };
        match five_bytes {
            None => dn,
            Some(false) => mem,
            Some(true) => mem5,
        }
    }

    /// Idle clocks of a bit field instruction on the 68030, like bf_idle_020()
    /// (MC68030UM 11.6.14)
    fn bf_idle_030(instr: &Instruction, five_bytes: Option<bool>) -> Ticks {
        // Dn, memory < 5 bytes, memory 5 bytes
        let (dn, mem, mem5) = match instr.mnemonic {
            InstructionMnemonic::BFTST => (4, 4, 6),
            InstructionMnemonic::BFCHG
            | InstructionMnemonic::BFCLR
            | InstructionMnemonic::BFSET => (10, 6, 10),
            InstructionMnemonic::BFEXTS | InstructionMnemonic::BFEXTU => (6, 6, 10),
            InstructionMnemonic::BFINS => (8, 4, 6),
            InstructionMnemonic::BFFFO => (16, 16, 20),
            _ => unreachable!(),
        };
        match five_bytes {
            None => dn,
            Some(false) => mem,
            Some(true) => mem5,
        }
    }

    /// Internal time of a bit field instruction. `field` is None for a data
    /// register, otherwise the field in memory: the address of its first byte,
    /// its offset in that byte (0-7) and its width.
    fn bf_cycles(
        &mut self,
        instr: &Instruction,
        sec: BfxExtWord,
        field: Option<(Address, i32, Long)>,
    ) -> Result<()> {
        let (five_bytes, spans) = match field {
            None => (None, None),
            Some((ea, offset, width)) => (
                Some(offset + width as i32 > 32),
                Some(Self::bf_spans040(ea, offset, width)),
            ),
        };
        advance_cycles!(
            self,
            m68000: 0,
            m68020: Self::bf_idle_020(instr, five_bytes),
            m68030: Self::bf_idle_030(instr, five_bytes),
        )?;
        account_040_time!(self, self.exec040_bf(instr, sec, spans)?);
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
                self.bf_cycles(instr, sec, None)?;
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

                let data_long = self.read_bitfield(ea, offset, width)?;
                self.regs
                    .sr
                    .set_n((data_long << (offset as isize)) & 0x80000000 != 0);
                self.regs.sr.set_z(data_long & mask_long == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                self.write_bitfield(ea, offset, width, data_long & !mask_long)?;

                // Handle bit fields that cross 32-bit boundaries
                let five_bytes = width as i32 + offset > 32;
                if five_bytes {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Update Z flag with the extended part
                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }

                    self.write_ticks(ea.wrapping_add(4), data_byte & !mask_byte)?;
                }
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let mut data = self.read_bitfield(ea, offset, width)?;
                data <<= offset as isize;

                // If the bit field crosses a 32-bit boundary, read an additional byte
                let five_bytes = offset + width as i32 > 32;
                if five_bytes {
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
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let mut data = self.read_bitfield(ea, offset, width)?;
                data <<= offset as isize;

                // If the bit field crosses a 32-bit boundary, read an additional byte
                let five_bytes = offset + width as i32 > 32;
                if five_bytes {
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
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let mut data = self.read_bitfield(ea, local_offset, width)?;
                data <<= local_offset as isize;

                // If the bit field crosses a 32-bit boundary, read an additional byte
                let five_bytes = local_offset + width as i32 > 32;
                if five_bytes {
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
                self.bf_cycles(instr, sec, Some((ea, local_offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let data_long = self.read_bitfield(ea, offset, width)?;

                self.regs
                    .sr
                    .set_n((data_long << (offset as isize)) & 0x80000000 != 0);
                self.regs.sr.set_z(data_long & mask_long == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                self.write_bitfield(ea, offset, width, data_long | mask_long)?;

                // Handle bit fields that cross 32-bit boundaries
                let five_bytes = width as i32 + offset > 32;
                if five_bytes {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Update Z flag with the extended part
                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }

                    self.write_ticks(ea.wrapping_add(4), data_byte | mask_byte)?;
                }
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let data_long = self.read_bitfield(ea, offset, width)?;

                let n_bit = (data_long & (0x80000000 >> (offset as isize))) != 0;
                self.regs.sr.set_n(n_bit);
                self.regs.sr.set_z(data_long & mask_long == 0);

                // Handle bit fields that cross 32-bit boundaries
                let five_bytes = width as i32 + offset > 32;
                if five_bytes {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }
                }

                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // No data modification for BFTST
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let data_long = self.read_bitfield(ea, offset, width)?;
                self.regs
                    .sr
                    .set_n((data_long << (offset as isize)) & 0x80000000 != 0);
                self.regs.sr.set_z(data_long & mask_long == 0);
                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // Write result back
                self.write_bitfield(ea, offset, width, data_long ^ mask_long)?;

                // Handle bit fields that cross 32-bit boundaries
                let five_bytes = width as i32 + offset > 32;
                if five_bytes {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Update Z flag with the extended part
                    if data_byte & mask_byte != 0 {
                        self.regs.sr.set_z(false);
                    }

                    // Write the extended part
                    self.write_ticks(ea.wrapping_add(4), data_byte ^ mask_byte)?;
                }
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
                self.bf_cycles(instr, sec, None)?;
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

                let data_long = self.read_bitfield(ea, offset, width)?;

                self.regs.sr.set_v(false);
                self.regs.sr.set_c(false);

                // Combine data and insert values and write back
                self.write_bitfield(ea, offset, width, (data_long & !mask_long) | insert_long)?;

                // Handle bit fields that cross 32-bit boundaries
                let five_bytes = width as i32 + offset > 32;
                if five_bytes {
                    let mask_byte = (mask_base as Byte) << (8 - offset);
                    let insert_byte = (insert_base as Byte) << (8 - offset);
                    let data_byte = self.read_ticks::<Byte>(ea.wrapping_add(4))?;

                    // Not updating Z flag here, it is based on the inserted data
                    // and was already set above.

                    // Write the extended part
                    self.write_ticks(ea.wrapping_add(4), (data_byte & !mask_byte) | insert_byte)?;
                }
                self.bf_cycles(instr, sec, Some((ea, offset, width)))?;
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
            a.wrapping_mul(b)
        } else {
            let a = self.regs.read_d::<Long>(extword.dl()) as u64;
            let b = self.read_ea::<Long>(instr, instr.get_op2())? as u64;
            a.wrapping_mul(b) as i64
        };

        // Computation time (maximum from the manuals, see op_mulu)
        advance_cycles!(self, m68030: 40, others: 39)?;

        self.prefetch_pump()?;

        self.regs.sr.set_c(false);
        if extword.size() {
            self.regs.sr.set_v(false);
            self.regs.sr.set_n(result < 0);
            self.regs.sr.set_z(result == 0);
        } else {
            // The flags are those of the 32 bit result. V is for a product
            // that does not fit in it.
            let low = result as i32;
            self.regs.sr.set_v(if extword.signed() {
                result != i64::from(low)
            } else {
                result as u64 > u64::from(u32::MAX)
            });
            self.regs.sr.set_n(low < 0);
            self.regs.sr.set_z(low == 0);
        }

        self.regs.write_d(extword.dl(), result as i32 as Long);
        if extword.size() {
            self.regs
                .write_d(extword.dh(), (result >> 32) as i32 as Long);
        }

        account_040_time!(
            self,
            if extword.signed() {
                timing040::MULS_L
            } else {
                timing040::MULU_L
            },
            instr.get_addr_mode()?,
        );
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
            // Division by zero. The manuals have no time for this on the 68020/68030;
            // take that of TRAPV, which stacks the same frame.
            advance_cycles!(self, m68000: 8, m68020: 3, m68030: 6)?;
            account_040_time!(self, timing040::DIV_L_ZERO);
            self.regs.sr.set_n(false);
            self.regs.sr.set_c(false);
            self.regs.sr.set_z(false);
            self.regs.sr.set_v(false);

            // Offset PC to the next instruction for the exception stack frame:
            // the prefetch queue holds one word less than a full one here
            self.regs.pc = self.regs.pc.wrapping_add(2);
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_DIV_ZERO, None);
        }

        account_040_time!(self, timing040::DIV_L, instr.get_addr_mode()?);
        self.prefetch_pump()?;

        let (quotient, remainder, overflow) = if extword.signed() {
            let a = if extword.size() {
                dividend as i64
            } else {
                i64::from(dividend as u32 as i32)
            };
            let b = i64::from(divisor as u32 as i32);
            // None for i64::MIN / -1, which overflows too
            match a.checked_div(b) {
                Some(q) => (q as u64, (a % b) as u64, i32::try_from(q).is_err()),
                None => (0, 0, true),
            }
        } else {
            let q = dividend / divisor;
            (q, dividend % divisor, q > u64::from(u32::MAX))
        };

        // Computation time (maximum from the manuals, see op_mulu)
        advance_cycles!(self, all: if extword.signed() { 86 } else { 74 })?;

        // The quotient must fit in 32 bits; the operands are left unchanged
        if overflow {
            trace!("DIV.l overflow");
            self.regs.sr.set_v(true);
            self.regs.sr.set_c(false);
            return Ok(());
        }

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
        advance_cycles!(self, m68000: 0, others: 5)?;

        if self.regs.sr.z() {
            self.write_ea(instr, instr.get_op2(), update_op)?;
        } else {
            self.regs.write_d(dc, ea_op);
        }

        account_040_time!(self, timing040::CAS, instr.get_addr_mode()?);
        Ok(())
    }

    /// CMP2 and CHK2: compare a register with the bounds pair at the
    /// effective address, the lower bound first. A data register is compared
    /// in its low part, an address register whole, against the bounds
    /// sign-extended to 32 bits. CHK2 traps when the register is out of
    /// bounds (M68000PRM CHK2, CMP2).
    fn op_cmp2<T: CpuSized>(&mut self, instr: &Instruction) -> Result<()> {
        // Control addressing modes only. Checked before fetching the extension
        // word, so that the illegal instruction exception stacks the address
        // of the instruction.
        if !matches!(
            instr.get_addr_mode()?,
            AddressingMode::Indirect
                | AddressingMode::IndirectDisplacement
                | AddressingMode::IndirectIndex
                | AddressingMode::IndirectIndexBase
                | AddressingMode::PCDisplacement
                | AddressingMode::PCIndex
                | AddressingMode::PCIndexBase
                | AddressingMode::AbsoluteShort
                | AddressingMode::AbsoluteLong
        ) {
            return self.raise_illegal_instruction();
        }
        let extword = self.fetch_pump()?;

        let ea = self.calc_ea_addr::<T>(instr, instr.get_addr_mode()?, instr.get_op2())?;
        let lower = self.read_ticks::<T>(ea)?;
        let upper = self.read_ticks::<T>(ea.wrapping_add(std::mem::size_of::<T>() as Address))?;

        let reg = usize::from((extword >> 12) & 0b111);
        let (value, lower, upper, mask) = if extword & (1 << 15) != 0 {
            (
                self.regs.read_a::<Long>(reg),
                lower.expand_signed() as Long,
                upper.expand_signed() as Long,
                Long::MAX,
            )
        } else {
            (
                self.regs.read_d::<T>(reg).expand(),
                lower.expand(),
                upper.expand(),
                T::chop(Long::MAX).expand(),
            )
        };

        // The bounds can be signed or unsigned, the lower bound being the
        // smaller one either way: the register is in bounds when its
        // distance above the lower bound is at most that of the upper one.
        let out = value.wrapping_sub(lower) & mask > upper.wrapping_sub(lower) & mask;
        self.regs.sr.set_z(value == lower || value == upper);
        self.regs.sr.set_c(out);

        // MC68020UM 8.2: 18 clocks in the cache case, 2 for each of the 2
        // instruction words, 3 for each of the 2 reads of the bounds and 8
        // internal
        advance_cycles!(self, all: 8)?;

        // On the 68040, three more clocks for CHK2 when UB < LB, which the manual
        // doesn't say how it compares: compared here as the values the instruction
        // compares unsigned. One more clock for an address register.
        account_040_time!(
            self,
            if extword & (1 << 11) == 0 {
                self.exec040_ea(&timing040::CMP2, instr.get_addr_mode()?)
            } else {
                self.exec040_ea(&timing040::CHK2, instr.get_addr_mode()?)
                    .plus(if upper < lower { 3 } else { 0 })
                    .plus(u16::from(extword & (1 << 15) != 0))
            },
        );

        if out && extword & (1 << 11) != 0 {
            // CHK2, with the next instruction in the exception frame
            self.prefetch_pump()?;
            return self.raise_exception(ExceptionGroup::Group2, VECTOR_CHK, None);
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
        // MC68020UM 8.2.16: 25 clocks, 22 when the compare fails. MC68030UM
        // 11.6.16: 24 either way.
        advance_cycles!(self, m68000: 0, m68020: 10, m68030: 9)?;
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

                account_040_time!(self, timing040::CAS2_TRUE);
                return Ok(());
            }
        }

        // Failed
        advance_cycles!(self, m68030: 3)?;
        // Write Dc1 last, as the PRM states that MemOp1 will be written to
        // the register if Dc1 == Dc2
        self.regs.write_d(dc2, mem_op2);
        self.regs.write_d(dc1, mem_op1);

        account_040_time!(self, timing040::CAS2_FALSE);
        Ok(())
    }
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
    TCaches,
> Tickable for CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU, TCaches>
where
    TBus: Bus<Address, u8> + IrqSource,
    TCaches: CpuCaches,
{
    fn tick(&mut self, _ticks: Ticks, _: ()) -> Result<Ticks> {
        self.step()?;

        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::access::{Bus68000, Bus68020};
    use crate::bus::testbus::Testbus;
    use crate::cpu_m68k::{CpuM68000, CpuM68020Fpu, M68000_ADDRESS_MASK, M68020_ADDRESS_MASK};

    use super::*;

    #[test]
    fn sr_68000() {
        let mut cpu =
            CpuM68000::<Testbus<Address, Byte, Bus68000>>::new(Testbus::new(M68000_ADDRESS_MASK));
        cpu.set_sr(0xFFFF);
        assert!(!cpu.regs.sr.m());
    }

    #[test]
    fn sr_68020() {
        let mut cpu = CpuM68020Fpu::<Testbus<Address, Byte, Bus68020>>::new(Testbus::new(
            M68020_ADDRESS_MASK,
        ));
        cpu.set_sr(0xFFFF);
        assert!(cpu.regs.sr.m());
    }
}
