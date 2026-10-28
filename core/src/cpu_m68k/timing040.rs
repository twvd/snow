//! Instruction times of the MC68040 integer unit (MC68040UM section 10)
//!
//! The 68040 runs instructions through a pipeline: the <ea> calculate and <ea>
//! fetch stages work on the next instructions while the execute stage runs the
//! current one. The tables give the time of each instruction in the execute
//! stage, for accesses that hit in the caches, as a lead time and a base time.
//! The lead time is how long the instruction waits for its operands to come
//! through the earlier stages, which a long enough previous instruction hides.
//! Bus cycles add their own time, like a cache miss does on the 68040 (10.1).

// The instructions start using the tables as they get their 68040 times
#![allow(dead_code)]

use anyhow::Result;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::instruction::{
    AddressingMode, BfxExtWord, ExtWord, Instruction, InstructionMnemonic, MemoryIndirectAction,
};
use crate::cpu_m68k::{CpuM68kType, FpuM68kType};
use crate::tickable::Ticks;
use crate::types::Long;

/// Execute stage time of an instruction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cpu_m68k) struct Exec040 {
    /// Clocks the instruction can wait for the earlier stages
    pub lead: u16,
    /// Clocks the instruction runs in the execute stage
    pub base: u16,
}

impl Exec040 {
    /// An instruction without an entry in the tables: one clock
    pub const DEFAULT: Self = e(0, 1);

    /// This time with `clocks` more base time
    pub const fn plus(self, clocks: u16) -> Self {
        e(self.lead, self.base + clocks)
    }
}

impl Default for Exec040 {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Lead time `lead` and base time `base`, written "lead L + base" in the manual
pub(in crate::cpu_m68k) const fn e(lead: u16, base: u16) -> Exec040 {
    Exec040 { lead, base }
}

/// Addressing modes the manual has no time for, because they aren't valid
/// for the instruction
const X: Exec040 = Exec040::DEFAULT;

/// Addressing modes of the rows of the tables
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cpu_m68k) enum Ea040 {
    Dn,
    An,
    /// (An)
    Ind,
    /// (An)+
    PostInc,
    /// -(An)
    PreDec,
    /// (d16,An)
    Disp,
    /// (d16,PC)
    PcDisp,
    /// (xxx).W, (xxx).L
    Abs,
    /// #<xxx>
    Imm,
    /// (d8,An,Xn)
    Index,
    /// (d8,PC,Xn)
    PcIndex,
    /// (BR,Xn)
    Base,
    /// (bd,BR,Xn)
    BaseDisp,
    /// ([bd,BR,Xn])
    PreIndex,
    /// ([bd,BR,Xn],od)
    PreIndexOd,
    /// ([bd,BR],Xn)
    PostIndex,
    /// ([bd,BR],Xn,od)
    PostIndexOd,
}

/// Number of rows of a table
const ROWS: usize = 17;

/// Times of an instruction by addressing mode, in the order of Ea040
pub(in crate::cpu_m68k) type Column040 = [Exec040; ROWS];

/// Addressing mode of a full format extension word (MC68040UM 10.1, note 1):
/// memory indirect with the index suppressed takes the time of pre-indexed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cpu_m68k) struct FullEa040 {
    row: Ea040,
    /// The base register is the PC: one more clock of lead time
    pc: bool,
}

impl FullEa040 {
    pub(in crate::cpu_m68k) fn new(
        extword: &ExtWord,
        indirect: MemoryIndirectAction,
        pc: bool,
    ) -> Self {
        let row = match indirect {
            MemoryIndirectAction::None if extword.full_displacement_size() == 0b01 => Ea040::Base,
            MemoryIndirectAction::None => Ea040::BaseDisp,
            MemoryIndirectAction::Null | MemoryIndirectAction::PreIndexNull => Ea040::PreIndex,
            MemoryIndirectAction::Word
            | MemoryIndirectAction::Long
            | MemoryIndirectAction::PreIndexWord
            | MemoryIndirectAction::PreIndexLong => Ea040::PreIndexOd,
            MemoryIndirectAction::PostIndexNull => Ea040::PostIndex,
            MemoryIndirectAction::PostIndexWord | MemoryIndirectAction::PostIndexLong => {
                Ea040::PostIndexOd
            }
        };
        Self { row, pc }
    }
}

/// MOVE (10.4), by source (rows) and destination (columns: Dn, (An), (An)+,
/// -(An), (d16,An), (xxx), (d8,An,Xn), (bd,An,Xn), ([bd,An,Xn]),
/// ([bd,An,Xn],od), ([bd,An],Xn), ([bd,An],Xn,od)). The manual has no row for
/// An and (BR,Xn) sources, which take the times of Dn and (bd,BR,Xn).
#[rustfmt::skip]
const MOVE: [[Exec040; 12]; 15] = [
    [e(0,  1), e(0,  1), e(0,  1), e(0,  1), e(0,  1), e(0,  1), e(0,  3), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // Dn
    [e(0,  1), e(0,  1), e(1,  1), e(1,  1), e(1,  1), e(0,  1), e(0,  4), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // (An)
    [e(0,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(0,  4), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // (An)+
    [e(0,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(0,  4), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // -(An)
    [e(0,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(0,  4), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // (d16,An)
    [e(2,  1), e(2,  1), e(2,  1), e(2,  1), e(3,  1), e(3,  1), e(4,  4), e(4,  6), e(4,  9), e(4, 10), e(6,  8), e(6,  9)], // (d16,PC)
    [e(0,  1), e(0,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(0,  4), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // (xxx)
    [e(0,  1), e(0,  1), e(1,  1), e(1,  1), e(1,  1), e(1,  1), e(0,  3), e(1,  6), e(1,  9), e(1, 10), e(3,  8), e(3,  9)], // #<xxx>
    [e(0,  3), e(0,  4), e(0,  5), e(0,  5), e(0,  5), e(0,  5), e(0,  8), e(0, 10), e(0, 13), e(0, 14), e(0, 14), e(0, 15)], // (d8,An,Xn)
    [e(1,  4), e(1,  4), e(1,  5), e(1,  5), e(1,  5), e(1,  5), e(1,  8), e(1, 10), e(1, 13), e(1, 14), e(1, 14), e(1, 15)], // (d8,PC,Xn)
    [e(1,  6), e(1,  6), e(1,  7), e(1,  7), e(1,  7), e(1,  7), e(1, 10), e(1, 12), e(1, 15), e(1, 16), e(1, 16), e(1, 17)], // (bd,BR,Xn)
    [e(1,  9), e(1,  9), e(1, 10), e(1, 10), e(1, 10), e(1, 10), e(1, 13), e(1, 15), e(1, 18), e(1, 19), e(1, 19), e(1, 20)], // ([bd,BR,Xn])
    [e(1, 10), e(1, 10), e(1, 11), e(1, 11), e(1, 11), e(1, 11), e(1, 14), e(1, 16), e(1, 19), e(1, 20), e(1, 20), e(1, 21)], // ([bd,BR,Xn],od)
    [e(3,  8), e(3,  8), e(3,  9), e(3,  9), e(3,  9), e(3,  9), e(3, 12), e(3, 14), e(3, 17), e(3, 18), e(3, 18), e(3, 19)], // ([bd,BR],Xn)
    [e(3,  9), e(3,  9), e(3, 10), e(3, 10), e(3, 10), e(3, 10), e(3, 13), e(3, 15), e(3, 18), e(3, 19), e(3, 19), e(3, 20)], // ([bd,BR],Xn,od)
];

/// MOVEA.L (10.6). MOVEA.W takes one more clock, but from Dn and #<xxx>.
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVEA_L: Column040 = [
    e(0,  1), // Dn
    e(0,  1), // An
    e(0,  1), // (An)
    e(0,  1), // (An)+
    e(0,  1), // -(An)
    e(0,  1), // (d16,An)
    e(2,  1), // (d16,PC)
    e(0,  1), // (xxx)
    e(0,  1), // #<xxx>
    e(0,  4), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 10), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3,  9), // ([bd,BR],Xn,od)
];

/// MOVE from CCR (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVE_FROM_CCR: Column040 = [
    e(0,  2), // Dn
    X,        // An
    e(0,  2), // (An)
    e(0,  2), // (An)+
    e(0,  2), // -(An)
    e(0,  2), // (d16,An)
    X,        // (d16,PC)
    e(0,  2), // (xxx)
    X,        // #<xxx>
    e(0,  4), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// MOVE to CCR (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVE_TO_CCR: Column040 = [
    e(0,  2), // Dn
    X,        // An
    e(0,  2), // (An)
    e(0,  2), // (An)+
    e(0,  2), // -(An)
    e(0,  2), // (d16,An)
    e(2,  2), // (d16,PC)
    e(0,  2), // (xxx)
    e(0,  2), // #<xxx>
    e(0,  4), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// MOVE from SR (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVE_FROM_SR: Column040 = [
    e(1,  2), // Dn
    X,        // An
    e(1,  2), // (An)
    e(1,  2), // (An)+
    e(1,  2), // -(An)
    e(1,  2), // (d16,An)
    X,        // (d16,PC)
    e(1,  2), // (xxx)
    X,        // #<xxx>
    e(0,  5), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// MOVE to SR (10.6, minimum times). The manual has no time for (BR,Xn),
/// which takes the time of (bd,BR,Xn).
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVE_TO_SR: Column040 = [
    e(1,  8), // Dn
    X,        // An
    e(2,  8), // (An)
    e(2,  8), // (An)+
    e(2,  8), // -(An)
    e(2,  8), // (d16,An)
    e(3,  8), // (d16,PC)
    e(2,  8), // (xxx)
    e(1,  8), // #<xxx>
    e(0, 11), // (d8,An,Xn)
    e(1, 11), // (d8,PC,Xn)
    e(1, 13), // (BR,Xn)
    e(1, 13), // (bd,BR,Xn)
    e(1, 16), // ([bd,BR,Xn])
    e(1, 17), // ([bd,BR,Xn],od)
    e(3, 15), // ([bd,BR],Xn)
    e(3, 16), // ([bd,BR],Xn,od)
];

/// MOVEQ (10.5)
pub(in crate::cpu_m68k) const MOVEQ: Exec040 = e(0, 1);
/// MOVE USP,An (10.5)
pub(in crate::cpu_m68k) const MOVE_FROM_USP: Exec040 = e(2, 1);
/// MOVE An,USP (10.5, minimum time)
pub(in crate::cpu_m68k) const MOVE_TO_USP: Exec040 = e(1, 6);

/// ADD, AND, EOR, OR, SUB <ea>,Dn and Dn,<ea>, TST (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const ALU: Column040 = [
    e(0,  1), // Dn
    e(0,  1), // An
    e(0,  1), // (An)
    e(0,  1), // (An)+
    e(0,  1), // -(An)
    e(0,  1), // (d16,An)
    e(2,  1), // (d16,PC)
    e(0,  1), // (xxx)
    e(0,  1), // #<xxx>
    e(0,  3), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// ADDA (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const ADDA: Column040 = [
    e(0,  2), // Dn
    e(0,  1), // An
    e(0,  2), // (An)
    e(1,  2), // (An)+
    e(1,  2), // -(An)
    e(1,  2), // (d16,An)
    e(2,  2), // (d16,PC)
    e(0,  2), // (xxx)
    e(0,  1), // #<xxx>
    e(0,  5), // (d8,An,Xn)
    e(1,  5), // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 12), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 11), // ([bd,BR],Xn,od)
];

/// ADDI, ANDI, EORI, ORI, SUBI (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const ALU_IMMEDIATE: Column040 = [
    e(0,  1), // Dn
    X,        // An
    e(0,  1), // (An)
    e(1,  1), // (An)+
    e(1,  1), // -(An)
    e(1,  1), // (d16,An)
    X,        // (d16,PC)
    e(1,  1), // (xxx)
    X,        // #<xxx>
    e(0,  3), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// ADDQ, SUBQ (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const ADDQ_SUBQ: Column040 = [
    e(0,  1), // Dn
    e(0,  1), // An
    e(0,  1), // (An)
    e(1,  1), // (An)+
    e(1,  1), // -(An)
    e(1,  1), // (d16,An)
    X,        // (d16,PC)
    e(0,  1), // (xxx)
    X,        // #<xxx>
    e(0,  3), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// CAS (10.6, typical times)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CAS: Column040 = [
    X,        // Dn
    X,        // An
    e(6, 31), // (An)
    e(5, 31), // (An)+
    e(5, 31), // -(An)
    e(5, 31), // (d16,An)
    X,        // (d16,PC)
    e(5, 31), // (xxx)
    X,        // #<xxx>
    e(0, 36), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1, 35), // (BR,Xn)
    e(1, 36), // (bd,BR,Xn)
    e(0, 40), // ([bd,BR,Xn])
    e(1, 41), // ([bd,BR,Xn],od)
    e(3, 38), // ([bd,BR],Xn)
    e(3, 39), // ([bd,BR],Xn,od)
];

/// CHK with Dn within bounds (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CHK: Column040 = [
    e(1,  7), // Dn
    X,        // An
    e(2,  7), // (An)
    e(2,  7), // (An)+
    e(2,  7), // -(An)
    e(2,  7), // (d16,An)
    e(3,  7), // (d16,PC)
    e(2,  7), // (xxx)
    e(1,  7), // #<xxx>
    e(0, 10), // (d8,An,Xn)
    e(1, 10), // (d8,PC,Xn)
    e(1, 11), // (BR,Xn)
    e(1, 12), // (bd,BR,Xn)
    e(1, 15), // ([bd,BR,Xn])
    e(1, 16), // ([bd,BR,Xn],od)
    e(3, 14), // ([bd,BR],Xn)
    e(3, 15), // ([bd,BR],Xn,od)
];

/// CHK2 with Rn within bounds, UB > LB and Rn = Dn (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CHK2: Column040 = [
    X,        // Dn
    X,        // An
    e(2,  9), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2,  9), // (d16,An)
    e(3,  9), // (d16,PC)
    e(2,  9), // (xxx)
    X,        // #<xxx>
    e(1, 12), // (d8,An,Xn)
    e(2, 12), // (d8,PC,Xn)
    e(2, 13), // (BR,Xn)
    e(2, 14), // (bd,BR,Xn)
    e(2, 17), // ([bd,BR,Xn])
    e(2, 18), // ([bd,BR,Xn],od)
    e(4, 16), // ([bd,BR],Xn)
    e(4, 17), // ([bd,BR],Xn,od)
];

/// CLR (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CLR: Column040 = [
    e(0,  1), // Dn
    X,        // An
    e(0,  1), // (An)
    e(0,  1), // (An)+
    e(0,  1), // -(An)
    e(0,  1), // (d16,An)
    X,        // (d16,PC)
    e(0,  1), // (xxx)
    X,        // #<xxx>
    e(0,  3), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  8), // ([bd,BR,Xn])
    e(1,  9), // ([bd,BR,Xn],od)
    e(3,  7), // ([bd,BR],Xn)
    e(3,  8), // ([bd,BR],Xn,od)
];

/// CMP (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CMP: Column040 = [
    e(0,  1), // Dn
    e(0,  1), // An
    e(0,  1), // (An)
    e(0,  1), // (An)+
    e(0,  1), // -(An)
    e(0,  1), // (d16,An)
    e(2,  1), // (d16,PC)
    e(0,  1), // (xxx)
    e(0,  1), // #<xxx>
    e(0,  3), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  8), // ([bd,BR,Xn])
    e(1,  9), // ([bd,BR,Xn],od)
    e(3,  7), // ([bd,BR],Xn)
    e(3,  8), // ([bd,BR],Xn,od)
];

/// CMPA.L (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CMPA_L: Column040 = [
    e(0,  1), // Dn
    e(0,  1), // An
    e(0,  1), // (An)
    e(1,  1), // (An)+
    e(1,  1), // -(An)
    e(1,  1), // (d16,An)
    e(2,  1), // (d16,PC)
    e(0,  1), // (xxx)
    e(0,  1), // #<xxx>
    e(0,  3), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  8), // ([bd,BR,Xn])
    e(1,  9), // ([bd,BR,Xn],od)
    e(3,  7), // ([bd,BR],Xn)
    e(3,  8), // ([bd,BR],Xn,od)
];

/// CMPI (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CMPI: Column040 = [
    e(0,  1), // Dn
    X,        // An
    e(0,  1), // (An)
    e(1,  1), // (An)+
    e(1,  1), // -(An)
    e(1,  1), // (d16,An)
    e(2,  1), // (d16,PC)
    e(1,  1), // (xxx)
    X,        // #<xxx>
    e(0,  3), // (d8,An,Xn)
    e(2,  4), // (d8,PC,Xn)
    e(2,  5), // (BR,Xn)
    e(2,  6), // (bd,BR,Xn)
    e(2,  8), // ([bd,BR,Xn])
    e(2,  9), // ([bd,BR,Xn],od)
    e(4,  7), // ([bd,BR],Xn)
    e(4,  8), // ([bd,BR],Xn,od)
];

/// CMP2 (10.6, typical times)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const CMP2: Column040 = [
    X,        // Dn
    X,        // An
    e(2, 11), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2, 11), // (d16,An)
    e(3, 11), // (d16,PC)
    e(2, 11), // (xxx)
    X,        // #<xxx>
    e(1, 14), // (d8,An,Xn)
    e(2, 14), // (d8,PC,Xn)
    e(2, 15), // (BR,Xn)
    e(2, 16), // (bd,BR,Xn)
    e(2, 19), // ([bd,BR,Xn])
    e(2, 20), // ([bd,BR,Xn],od)
    e(4, 18), // ([bd,BR],Xn)
    e(4, 19), // ([bd,BR],Xn,od)
];

/// NBCD (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const NBCD: Column040 = [
    e(0,  3), // Dn
    X,        // An
    e(0,  2), // (An)
    e(0,  2), // (An)+
    e(0,  2), // -(An)
    e(0,  2), // (d16,An)
    X,        // (d16,PC)
    e(0,  2), // (xxx)
    X,        // #<xxx>
    e(0,  4), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 10), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3,  9), // ([bd,BR],Xn,od)
];

/// NEG, NEGX, NOT (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const NEG: Column040 = [
    e(0,  1), // Dn
    X,        // An
    e(0,  1), // (An)
    e(0,  1), // (An)+
    e(0,  1), // -(An)
    e(0,  1), // (d16,An)
    X,        // (d16,PC)
    e(0,  1), // (xxx)
    X,        // #<xxx>
    e(0,  3), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  8), // ([bd,BR,Xn])
    e(1,  9), // ([bd,BR,Xn],od)
    e(3,  7), // ([bd,BR],Xn)
    e(3,  8), // ([bd,BR],Xn,od)
];

/// Scc (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const SCC: Column040 = [
    e(0,  2), // Dn
    X,        // An
    e(0,  2), // (An)
    e(0,  2), // (An)+
    e(0,  2), // -(An)
    e(0,  2), // (d16,An)
    X,        // (d16,PC)
    e(0,  2), // (xxx)
    X,        // #<xxx>
    e(0,  5), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// SUBA (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const SUBA: Column040 = [
    e(0,  1), // Dn
    e(0,  2), // An
    e(0,  2), // (An)
    e(1,  2), // (An)+
    e(1,  2), // -(An)
    e(1,  2), // (d16,An)
    e(2,  2), // (d16,PC)
    e(0,  2), // (xxx)
    e(0,  2), // #<xxx>
    e(0,  5), // (d8,An,Xn)
    e(1,  5), // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 10), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3,  9), // ([bd,BR],Xn,od)
];

/// TAS (10.6, typical times)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const TAS: Column040 = [
    e(0,  2), // Dn
    X,        // An
    e(2, 24), // (An)
    e(2, 24), // (An)+
    e(2, 24), // -(An)
    e(2, 24), // (d16,An)
    X,        // (d16,PC)
    e(2, 24), // (xxx)
    X,        // #<xxx>
    e(0, 27), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1, 28), // (BR,Xn)
    e(1, 29), // (bd,BR,Xn)
    e(0, 33), // ([bd,BR,Xn])
    e(0, 34), // ([bd,BR,Xn],od)
    e(3, 31), // ([bd,BR],Xn)
    e(3, 32), // ([bd,BR],Xn,od)
];

/// ABCD, SBCD Dy,Dx (10.5)
pub(in crate::cpu_m68k) const BCD_DN: Exec040 = e(0, 3);
/// ABCD, SBCD -(Ay),-(Ax) (10.5)
pub(in crate::cpu_m68k) const BCD_PREDEC: Exec040 = e(1, 3);
/// ADDX, SUBX Dy,Dx (10.5)
pub(in crate::cpu_m68k) const ADDX_DN: Exec040 = e(0, 1);
/// ADDX, SUBX -(Ay),-(Ax) (10.5)
pub(in crate::cpu_m68k) const ADDX_PREDEC: Exec040 = e(1, 2);
/// ANDI, EORI, ORI to CCR (10.5)
pub(in crate::cpu_m68k) const BITWISE_CCR: Exec040 = e(0, 4);
/// ANDI, EORI, ORI to SR (10.5, minimum time)
pub(in crate::cpu_m68k) const BITWISE_SR: Exec040 = e(1, 8);
/// CAS2 when both compares are equal (10.5, typical time)
pub(in crate::cpu_m68k) const CAS2_TRUE: Exec040 = e(6, 49);
/// CAS2 when a compare isn't equal (10.5, typical time)
pub(in crate::cpu_m68k) const CAS2_FALSE: Exec040 = e(6, 44);
/// CMPM (10.5)
pub(in crate::cpu_m68k) const CMPM: Exec040 = e(1, 2);
/// EXG Dy,Dx and Dy,Ax (10.5)
pub(in crate::cpu_m68k) const EXG: Exec040 = e(0, 1);
/// EXG Ay,Ax (10.5)
pub(in crate::cpu_m68k) const EXG_AN: Exec040 = e(1, 1);
/// EXT.W (10.5)
pub(in crate::cpu_m68k) const EXT_W: Exec040 = e(0, 2);
/// EXT.L, EXTB.L (10.5)
pub(in crate::cpu_m68k) const EXT_L: Exec040 = e(0, 1);
/// PACK Dx,Dy (10.5)
pub(in crate::cpu_m68k) const PACK_DN: Exec040 = e(0, 3);
/// PACK -(Ax),-(Ay) (10.5)
pub(in crate::cpu_m68k) const PACK_PREDEC: Exec040 = e(2, 3);
/// SWAP (10.5)
pub(in crate::cpu_m68k) const SWAP: Exec040 = e(0, 2);
/// UNPK Dx,Dy (10.5)
pub(in crate::cpu_m68k) const UNPK_DN: Exec040 = e(0, 4);
/// UNPK -(Ax),-(Ay) (10.5)
pub(in crate::cpu_m68k) const UNPK_PREDEC: Exec040 = e(2, 4);

/// ASL (10.6), Dn with the count in the instruction
#[rustfmt::skip]
const ASL: Column040 = [
    e(0,  3), // Dn
    X,        // An
    e(0,  3), // (An)
    e(0,  3), // (An)+
    e(0,  3), // -(An)
    e(0,  3), // (d16,An)
    X,        // (d16,PC)
    e(0,  3), // (xxx)
    X,        // #<xxx>
    e(0,  5), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  8), // (BR,Xn)
    e(1,  9), // (bd,BR,Xn)
    e(1, 11), // ([bd,BR,Xn])
    e(1, 12), // ([bd,BR,Xn],od)
    e(3, 10), // ([bd,BR],Xn)
    e(3, 11), // ([bd,BR],Xn,od)
];

/// ASR, LSL, LSR (10.6), Dn with the count in the instruction
#[rustfmt::skip]
const SHIFT: Column040 = [
    e(0,  2), // Dn
    X,        // An
    e(0,  2), // (An)
    e(0,  2), // (An)+
    e(0,  2), // -(An)
    e(0,  2), // (d16,An)
    X,        // (d16,PC)
    e(0,  2), // (xxx)
    X,        // #<xxx>
    e(0,  4), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  7), // (BR,Xn)
    e(1,  8), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// ROL, ROR (10.6), Dn with the count in the instruction
#[rustfmt::skip]
const ROTATE: Column040 = [
    e(0,  3), // Dn
    X,        // An
    e(0,  3), // (An)
    e(0,  3), // (An)+
    e(0,  3), // -(An)
    e(0,  3), // (d16,An)
    X,        // (d16,PC)
    e(0,  3), // (xxx)
    X,        // #<xxx>
    e(0,  5), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  7), // (BR,Xn)
    e(1,  8), // (bd,BR,Xn)
    e(1, 10), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3,  9), // ([bd,BR],Xn)
    e(3, 10), // ([bd,BR],Xn,od)
];

/// ROXL, ROXR (10.6), Dn with the count in the instruction
#[rustfmt::skip]
const ROTATE_X: Column040 = [
    e(0,  5), // Dn
    X,        // An
    e(0,  2), // (An)
    e(0,  2), // (An)+
    e(0,  2), // -(An)
    e(0,  2), // (d16,An)
    X,        // (d16,PC)
    e(0,  2), // (xxx)
    X,        // #<xxx>
    e(0,  4), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 10), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3,  9), // ([bd,BR],Xn,od)
];

/// Column of a shift or rotate instruction `mnemonic`. The register forms
/// with the count in a register take one more clock.
pub(in crate::cpu_m68k) const fn shift_column(mnemonic: InstructionMnemonic) -> &'static Column040 {
    use InstructionMnemonic::*;
    match mnemonic {
        ASL_b | ASL_w | ASL_l | ASL_ea => &ASL,
        ROL_b | ROL_w | ROL_l | ROL_ea | ROR_b | ROR_w | ROR_l | ROR_ea => &ROTATE,
        ROXL_b | ROXL_w | ROXL_l | ROXL_ea | ROXR_b | ROXR_w | ROXR_l | ROXR_ea => &ROTATE_X,
        _ => &SHIFT,
    }
}

/// MULS.W (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MULS_W: Column040 = [
    e(0, 16), // Dn
    X,        // An
    e(0, 16), // (An)
    e(0, 16), // (An)+
    e(0, 16), // -(An)
    e(0, 16), // (d16,An)
    e(2, 16), // (d16,PC)
    e(0, 16), // (xxx)
    e(0, 16), // #<xxx>
    e(0, 18), // (d8,An,Xn)
    e(1, 19), // (d8,PC,Xn)
    e(1, 20), // (BR,Xn)
    e(1, 21), // (bd,BR,Xn)
    e(1, 23), // ([bd,BR,Xn])
    e(1, 24), // ([bd,BR,Xn],od)
    e(3, 22), // ([bd,BR],Xn)
    e(3, 23), // ([bd,BR],Xn,od)
];

/// MULS.L (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MULS_L: Column040 = [
    e(0, 20), // Dn
    X,        // An
    e(0, 20), // (An)
    e(0, 20), // (An)+
    e(0, 20), // -(An)
    e(0, 20), // (d16,An)
    e(2, 20), // (d16,PC)
    e(0, 20), // (xxx)
    e(0, 20), // #<xxx>
    e(0, 22), // (d8,An,Xn)
    e(1, 23), // (d8,PC,Xn)
    e(1, 24), // (BR,Xn)
    e(1, 25), // (bd,BR,Xn)
    e(1, 27), // ([bd,BR,Xn])
    e(1, 28), // ([bd,BR,Xn],od)
    e(3, 26), // ([bd,BR],Xn)
    e(3, 27), // ([bd,BR],Xn,od)
];

/// MULU.W (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MULU_W: Column040 = [
    e(0, 14), // Dn
    X,        // An
    e(0, 14), // (An)
    e(0, 14), // (An)+
    e(0, 14), // -(An)
    e(0, 14), // (d16,An)
    e(0, 14), // (d16,PC)
    e(0, 14), // (xxx)
    e(0, 14), // #<xxx>
    e(0, 16), // (d8,An,Xn)
    e(1, 17), // (d8,PC,Xn)
    e(1, 18), // (BR,Xn)
    e(1, 19), // (bd,BR,Xn)
    e(1, 21), // ([bd,BR,Xn])
    e(1, 22), // ([bd,BR,Xn],od)
    e(3, 20), // ([bd,BR],Xn)
    e(3, 21), // ([bd,BR],Xn,od)
];

/// MULU.L (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MULU_L: Column040 = [
    e(0, 20), // Dn
    X,        // An
    e(0, 20), // (An)
    e(0, 20), // (An)+
    e(0, 20), // -(An)
    e(0, 20), // (d16,An)
    e(0, 20), // (d16,PC)
    e(0, 20), // (xxx)
    e(0, 20), // #<xxx>
    e(0, 22), // (d8,An,Xn)
    e(1, 23), // (d8,PC,Xn)
    e(1, 24), // (BR,Xn)
    e(1, 25), // (bd,BR,Xn)
    e(1, 27), // ([bd,BR,Xn])
    e(1, 28), // ([bd,BR,Xn],od)
    e(3, 26), // ([bd,BR],Xn)
    e(3, 27), // ([bd,BR],Xn,od)
];

/// DIVS.W, DIVU.W (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const DIV_W: Column040 = [
    e(0, 27), // Dn
    X,        // An
    e(0, 27), // (An)
    e(0, 27), // (An)+
    e(0, 27), // -(An)
    e(0, 27), // (d16,An)
    e(3, 27), // (d16,PC)
    e(0, 27), // (xxx)
    e(0, 27), // #<xxx>
    e(0, 30), // (d8,An,Xn)
    e(1, 30), // (d8,PC,Xn)
    e(1, 31), // (BR,Xn)
    e(1, 32), // (bd,BR,Xn)
    e(1, 35), // ([bd,BR,Xn])
    e(1, 36), // ([bd,BR,Xn],od)
    e(3, 34), // ([bd,BR],Xn)
    e(3, 35), // ([bd,BR],Xn,od)
];

/// DIVS.L, DIVU.L, DIVSL.L, DIVUL.L (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const DIV_L: Column040 = [
    e(0, 44), // Dn
    X,        // An
    e(0, 44), // (An)
    e(0, 44), // (An)+
    e(0, 44), // -(An)
    e(2, 44), // (d16,An)
    e(3, 44), // (d16,PC)
    e(2, 44), // (xxx)
    e(1, 44), // #<xxx>
    e(0, 47), // (d8,An,Xn)
    e(1, 47), // (d8,PC,Xn)
    e(1, 48), // (BR,Xn)
    e(1, 49), // (bd,BR,Xn)
    e(1, 52), // ([bd,BR,Xn])
    e(1, 53), // ([bd,BR,Xn],od)
    e(3, 51), // ([bd,BR],Xn)
    e(3, 52), // ([bd,BR],Xn,od)
];

/// BCHG, BCLR, BSET with an immediate bit number (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const BIT_CHANGE: Column040 = [
    e(0,  3), // Dn
    X,        // An
    e(0,  3), // (An)
    e(0,  3), // (An)+
    e(0,  3), // -(An)
    e(1,  3), // (d16,An)
    X,        // (d16,PC)
    e(1,  3), // (xxx)
    X,        // #<xxx>
    e(0,  5), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  8), // (BR,Xn)
    e(1,  9), // (bd,BR,Xn)
    e(1, 11), // ([bd,BR,Xn])
    e(1, 12), // ([bd,BR,Xn],od)
    e(3, 10), // ([bd,BR],Xn)
    e(3, 11), // ([bd,BR],Xn,od)
];

/// BTST with an immediate bit number (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const BTST: Column040 = [
    e(0,  1), // Dn
    X,        // An
    e(0,  1), // (An)
    e(0,  1), // (An)+
    e(0,  1), // -(An)
    e(1,  1), // (d16,An)
    e(2,  1), // (d16,PC)
    e(1,  1), // (xxx)
    X,        // #<xxx>
    e(0,  3), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  6), // (BR,Xn)
    e(1,  7), // (bd,BR,Xn)
    e(1,  9), // ([bd,BR,Xn])
    e(1, 10), // ([bd,BR,Xn],od)
    e(3,  8), // ([bd,BR],Xn)
    e(3,  9), // ([bd,BR],Xn,od)
];

/// BFCHG, BFCLR, BFSET (10.6), Dn with an immediate offset and width
#[rustfmt::skip]
const BF_CHANGE: Column040 = [
    e(0,  6), // Dn
    X,        // An
    e(2,  8), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2,  8), // (d16,An)
    X,        // (d16,PC)
    e(2,  8), // (xxx)
    X,        // #<xxx>
    e(0, 11), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1, 13), // (BR,Xn)
    e(1, 14), // (bd,BR,Xn)
    e(1, 16), // ([bd,BR,Xn])
    e(1, 17), // ([bd,BR,Xn],od)
    e(3, 15), // ([bd,BR],Xn)
    e(3, 16), // ([bd,BR],Xn,od)
];

/// BFEXTS, BFEXTU (10.6), Dn with an immediate offset and width
#[rustfmt::skip]
const BF_EXTRACT: Column040 = [
    e(0,  4), // Dn
    X,        // An
    e(2,  7), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2,  7), // (d16,An)
    e(3,  7), // (d16,PC)
    e(2,  7), // (xxx)
    X,        // #<xxx>
    e(0, 10), // (d8,An,Xn)
    e(1, 10), // (d8,PC,Xn)
    e(1, 12), // (BR,Xn)
    e(1, 13), // (bd,BR,Xn)
    e(1, 15), // ([bd,BR,Xn])
    e(1, 16), // ([bd,BR,Xn],od)
    e(3, 14), // ([bd,BR],Xn)
    e(3, 15), // ([bd,BR],Xn,od)
];

/// BFFFO (10.6), Dn with an immediate offset and width
#[rustfmt::skip]
const BFFFO: Column040 = [
    e(0,  6), // Dn
    X,        // An
    e(2,  9), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2,  9), // (d16,An)
    e(3,  9), // (d16,PC)
    e(2,  9), // (xxx)
    X,        // #<xxx>
    e(0, 12), // (d8,An,Xn)
    e(1, 12), // (d8,PC,Xn)
    e(1, 14), // (BR,Xn)
    e(1, 15), // (bd,BR,Xn)
    e(1, 17), // ([bd,BR,Xn])
    e(1, 18), // ([bd,BR,Xn],od)
    e(3, 16), // ([bd,BR],Xn)
    e(3, 17), // ([bd,BR],Xn,od)
];

/// BFINS (10.6), Dn with an immediate offset and width
#[rustfmt::skip]
const BFINS: Column040 = [
    e(0,  5), // Dn
    X,        // An
    e(2,  7), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2,  7), // (d16,An)
    X,        // (d16,PC)
    e(2,  7), // (xxx)
    X,        // #<xxx>
    e(0, 10), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1, 12), // (BR,Xn)
    e(1, 13), // (bd,BR,Xn)
    e(1, 15), // ([bd,BR,Xn])
    e(1, 16), // ([bd,BR,Xn],od)
    e(3, 14), // ([bd,BR],Xn)
    e(3, 15), // ([bd,BR],Xn,od)
];

/// BFTST (10.6), Dn with an immediate offset and width
#[rustfmt::skip]
const BFTST: Column040 = [
    e(0,  3), // Dn
    X,        // An
    e(2,  7), // (An)
    X,        // (An)+
    X,        // -(An)
    e(2,  7), // (d16,An)
    e(3,  7), // (d16,PC)
    e(2,  7), // (xxx)
    X,        // #<xxx>
    e(0, 10), // (d8,An,Xn)
    e(1, 10), // (d8,PC,Xn)
    e(1, 12), // (BR,Xn)
    e(1, 13), // (bd,BR,Xn)
    e(1, 15), // ([bd,BR,Xn])
    e(1, 16), // ([bd,BR,Xn],od)
    e(3, 14), // ([bd,BR],Xn)
    e(3, 15), // ([bd,BR],Xn,od)
];

/// JMP (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const JMP: Column040 = [
    X,        // Dn
    X,        // An
    e(2,  1), // (An)
    X,        // (An)+
    X,        // -(An)
    e(3,  1), // (d16,An)
    e(5,  1), // (d16,PC)
    e(2,  1), // (xxx)
    X,        // #<xxx>
    e(0,  6), // (d8,An,Xn)
    e(1,  6), // (d8,PC,Xn)
    e(1,  7), // (BR,Xn)
    e(1,  8), // (bd,BR,Xn)
    e(1, 11), // ([bd,BR,Xn])
    e(1, 11), // ([bd,BR,Xn],od)
    e(3, 10), // ([bd,BR],Xn)
    e(3, 11), // ([bd,BR],Xn,od)
];

/// JSR (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const JSR: Column040 = [
    X,        // Dn
    X,        // An
    e(2,  1), // (An)
    X,        // (An)+
    X,        // -(An)
    e(3,  1), // (d16,An)
    e(5,  1), // (d16,PC)
    e(2,  1), // (xxx)
    X,        // #<xxx>
    e(0,  6), // (d8,An,Xn)
    e(1,  6), // (d8,PC,Xn)
    e(1,  7), // (BR,Xn)
    e(1,  8), // (bd,BR,Xn)
    e(1, 11), // ([bd,BR,Xn])
    e(1, 12), // ([bd,BR,Xn],od)
    e(3, 10), // ([bd,BR],Xn)
    e(3, 11), // ([bd,BR],Xn,od)
];

/// LEA (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const LEA: Column040 = [
    X,        // Dn
    X,        // An
    e(0,  1), // (An)
    X,        // (An)+
    X,        // -(An)
    e(1,  1), // (d16,An)
    e(3,  1), // (d16,PC)
    e(0,  1), // (xxx)
    X,        // #<xxx>
    e(0,  4), // (d8,An,Xn)
    e(1,  4), // (d8,PC,Xn)
    e(1,  5), // (BR,Xn)
    e(1,  6), // (bd,BR,Xn)
    e(1,  8), // ([bd,BR,Xn])
    e(1,  9), // ([bd,BR,Xn],od)
    e(3,  7), // ([bd,BR],Xn)
    e(3,  8), // ([bd,BR],Xn,od)
];

/// PEA (10.6)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const PEA: Column040 = [
    X,        // Dn
    X,        // An
    e(1,  1), // (An)
    X,        // (An)+
    X,        // -(An)
    e(1,  1), // (d16,An)
    e(3,  1), // (d16,PC)
    e(1,  1), // (xxx)
    X,        // #<xxx>
    e(1,  3), // (d8,An,Xn)
    e(2,  4), // (d8,PC,Xn)
    e(2,  5), // (BR,Xn)
    e(2,  6), // (bd,BR,Xn)
    e(2,  8), // ([bd,BR,Xn])
    e(2,  9), // ([bd,BR,Xn],od)
    e(4,  7), // ([bd,BR],Xn)
    e(4,  8), // ([bd,BR],Xn,od)
];

/// Bcc, BRA when the branch is taken (10.5)
pub(in crate::cpu_m68k) const BCC_TAKEN: Exec040 = e(0, 2);
/// Bcc when the branch isn't taken (10.5)
pub(in crate::cpu_m68k) const BCC_NOT_TAKEN: Exec040 = e(0, 3);
/// BSR (10.5)
pub(in crate::cpu_m68k) const BSR: Exec040 = e(1, 1);
/// DBcc when the condition is false and the count isn't -1: the branch is
/// taken (10.5)
pub(in crate::cpu_m68k) const DBCC_TAKEN: Exec040 = e(0, 3);
/// DBcc when the condition is true, or the count is -1 (10.5)
pub(in crate::cpu_m68k) const DBCC_NOT_TAKEN: Exec040 = e(0, 4);
/// LINK (10.5)
pub(in crate::cpu_m68k) const LINK: Exec040 = e(2, 1);
/// NOP (10.5, minimum time): it synchronizes the pipeline
pub(in crate::cpu_m68k) const NOP: Exec040 = e(1, 7);
/// RTD (10.5)
pub(in crate::cpu_m68k) const RTD: Exec040 = e(1, 5);
/// RTR (10.5)
pub(in crate::cpu_m68k) const RTR: Exec040 = e(1, 6);
/// RTS (10.5)
pub(in crate::cpu_m68k) const RTS: Exec040 = e(0, 5);
/// UNLK (10.5)
pub(in crate::cpu_m68k) const UNLK: Exec040 = e(1, 1);

/// RTE through a frame of format `format` (10.5, minimum times). Format $1
/// is the time of both frames.
pub(in crate::cpu_m68k) const fn rte(format: u16) -> Exec040 {
    match format {
        0x0 => e(0, 13),
        0x1 => e(0, 23),
        0x2 => e(0, 14),
        0x3 => e(0, 20),
        0x4 => e(0, 15),
        0x7 => e(0, 23),
        _ => Exec040::DEFAULT,
    }
}

/// MOVEM <list>,<ea> (10.6), without the registers
#[rustfmt::skip]
const MOVEM_TO_MEM: Column040 = [
    X,        // Dn
    X,        // An
    e(1,  1), // (An)
    X,        // (An)+
    e(1,  1), // -(An)
    e(1,  1), // (d16,An)
    X,        // (d16,PC)
    e(1,  1), // (xxx)
    X,        // #<xxx>
    e(2,  7), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(3,  8), // (BR,Xn)
    e(3,  9), // (bd,BR,Xn)
    e(3, 12), // ([bd,BR,Xn])
    e(3, 13), // ([bd,BR,Xn],od)
    e(5, 11), // ([bd,BR],Xn)
    e(5, 12), // ([bd,BR],Xn,od)
];

/// MOVEM.L <ea>,<list> (10.6), without the registers
#[rustfmt::skip]
const MOVEM_TO_REGS: Column040 = [
    X,        // Dn
    X,        // An
    e(1,  2), // (An)
    e(1,  2), // (An)+
    X,        // -(An)
    e(1,  2), // (d16,An)
    e(2,  2), // (d16,PC)
    e(1,  2), // (xxx)
    X,        // #<xxx>
    e(2,  8), // (d8,An,Xn)
    e(3,  8), // (d8,PC,Xn)
    e(3,  9), // (BR,Xn)
    e(3, 10), // (bd,BR,Xn)
    e(3, 13), // ([bd,BR,Xn])
    e(3, 14), // ([bd,BR,Xn],od)
    e(5, 12), // ([bd,BR],Xn)
    e(5, 13), // ([bd,BR],Xn,od)
];

/// MOVES <ea>,An (10.6, typical times)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVES_TO_AN: Column040 = [
    X,        // Dn
    X,        // An
    e(4, 24), // (An)
    e(4, 24), // (An)+
    e(2, 15), // -(An)
    e(4, 24), // (d16,An)
    X,        // (d16,PC)
    e(2, 15), // (xxx)
    X,        // #<xxx>
    e(1, 27), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(2, 19), // (BR,Xn)
    e(2, 20), // (bd,BR,Xn)
    e(2, 32), // ([bd,BR,Xn])
    e(2, 29), // ([bd,BR,Xn],od)
    e(4, 31), // ([bd,BR],Xn)
    e(4, 28), // ([bd,BR],Xn,od)
];

/// MOVES <ea>,Dn (10.6, typical times)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVES_TO_DN: Column040 = [
    X,        // Dn
    X,        // An
    e(4, 19), // (An)
    e(4, 19), // (An)+
    e(0, 12), // -(An)
    e(4, 19), // (d16,An)
    X,        // (d16,PC)
    e(4, 10), // (xxx)
    X,        // #<xxx>
    e(1, 22), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(2, 14), // (BR,Xn)
    e(2, 15), // (bd,BR,Xn)
    e(2, 27), // ([bd,BR,Xn])
    e(2, 24), // ([bd,BR,Xn],od)
    e(4, 26), // ([bd,BR],Xn)
    e(4, 23), // ([bd,BR],Xn,od)
];

/// MOVES Rn,<ea> (10.6, typical times)
#[rustfmt::skip]
pub(in crate::cpu_m68k) const MOVES_TO_MEM: Column040 = [
    X,        // Dn
    X,        // An
    e(4,  9), // (An)
    e(4,  9), // (An)+
    e(2,  9), // -(An)
    e(4,  9), // (d16,An)
    X,        // (d16,PC)
    e(2,  9), // (xxx)
    X,        // #<xxx>
    e(1, 12), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(2, 13), // (BR,Xn)
    e(2, 14), // (bd,BR,Xn)
    e(2, 17), // ([bd,BR,Xn])
    e(2, 18), // ([bd,BR,Xn],od)
    e(4, 16), // ([bd,BR],Xn)
    e(4, 17), // ([bd,BR],Xn,od)
];

/// MOVEC Rn,Rc (10.5, typical time)
pub(in crate::cpu_m68k) const MOVEC_TO_CTRL: Exec040 = e(1, 6);
/// MOVEC Rc,Rn (10.5, typical time)
pub(in crate::cpu_m68k) const MOVEC_FROM_CTRL: Exec040 = e(1, 10);
/// MOVEP.W Dn,(d16,An) (10.5)
pub(in crate::cpu_m68k) const MOVEP_W_TO_MEM: Exec040 = e(2, 9);
/// MOVEP.L Dn,(d16,An) (10.5)
pub(in crate::cpu_m68k) const MOVEP_L_TO_MEM: Exec040 = e(2, 11);
/// MOVEP.W (d16,An),Dn (10.5)
pub(in crate::cpu_m68k) const MOVEP_W_TO_DN: Exec040 = e(2, 5);
/// MOVEP.L (d16,An),Dn (10.5)
pub(in crate::cpu_m68k) const MOVEP_L_TO_DN: Exec040 = e(2, 8);
/// MOVE16 (Ax)+,(Ay)+ (10.5). The manual adds 8 clocks to each MOVE16 that
/// follows another, which isn't modelled.
pub(in crate::cpu_m68k) const MOVE16_INC: Exec040 = e(1, 7);
/// MOVE16 xxx.L,(An)+ (10.5)
pub(in crate::cpu_m68k) const MOVE16_TO_AN_INC: Exec040 = e(0, 8);
/// MOVE16 xxx.L,(An), (An),xxx.L and (An)+,xxx.L (10.5)
pub(in crate::cpu_m68k) const MOVE16_ABS: Exec040 = e(0, 7);
/// PFLUSH, PFLUSHN and PFLUSHA (10.5, typical time)
pub(in crate::cpu_m68k) const PFLUSH: Exec040 = e(1, 10);
/// PFLUSHAN (10.5, typical time)
pub(in crate::cpu_m68k) const PFLUSHAN: Exec040 = e(1, 26);
/// PTESTR, PTESTW (10.5): a typical three-level table search without
/// descriptor writes, which includes the time of the descriptor reads that
/// the bus adds as well
pub(in crate::cpu_m68k) const PTEST: Exec040 = e(11, 14);
/// RESET (10.5, minimum time)
pub(in crate::cpu_m68k) const RESET: Exec040 = e(0, 521);

/// Exception processing (10.5): the time of TRAP #n, which is the only
/// exception the manual times on its own. Every exception takes it, the
/// instructions that trap only add their own time.
pub(in crate::cpu_m68k) const EXCEPTION: Ticks = 16;
/// TRAP, ILLEGAL, A-line, F-line and a privilege violation: the exception
/// alone (10.5)
pub(in crate::cpu_m68k) const TRAP: Exec040 = e(0, 0);
/// TRAPcc, TRAPV when the trap is taken: 19 clocks with the exception (10.5)
pub(in crate::cpu_m68k) const TRAPCC_TAKEN: Exec040 = e(0, 3);
/// TRAPcc, TRAPV when the trap isn't taken (10.5)
pub(in crate::cpu_m68k) const TRAPCC_NOT_TAKEN: Exec040 = e(0, 5);
/// DIVS.W, DIVU.W by zero: about the exception and the <ea> calculate time
/// (10.6), taken for a data register divisor
pub(in crate::cpu_m68k) const DIV_W_ZERO: Exec040 = e(0, 8);
/// DIVS.L, DIVU.L by zero, like DIV_W_ZERO
pub(in crate::cpu_m68k) const DIV_L_ZERO: Exec040 = e(0, 9);

/// FScc (10.7.2), for an idle FPU
#[rustfmt::skip]
pub(in crate::cpu_m68k) const FSCC: Column040 = [
    e(0,  6), // Dn
    X,        // An
    e(0,  5), // (An)
    e(2,  5), // (An)+
    e(2,  5), // -(An)
    e(0,  5), // (d16,An)
    X,        // (d16,PC)
    e(0,  5), // (xxx)
    X,        // #<xxx>
    e(0,  8), // (d8,An,Xn)
    X,        // (d8,PC,Xn)
    e(1,  9), // (BR,Xn)
    e(1, 10), // (bd,BR,Xn)
    e(1, 13), // ([bd,BR,Xn])
    e(1, 14), // ([bd,BR,Xn],od)
    e(3, 12), // ([bd,BR],Xn)
    e(3, 13), // ([bd,BR],Xn,od)
];

/// FBcc when the branch is taken (10.7.1)
pub(in crate::cpu_m68k) const FBCC_TAKEN: Exec040 = e(0, 7);
/// FBcc when the branch isn't taken (10.7.1)
pub(in crate::cpu_m68k) const FBCC_NOT_TAKEN: Exec040 = e(0, 6);
/// FDBcc when the condition is true (10.7.1)
pub(in crate::cpu_m68k) const FDBCC_TRUE: Exec040 = e(1, 7);
/// FDBcc when the condition is false (10.7.1)
pub(in crate::cpu_m68k) const FDBCC_FALSE: Exec040 = e(1, 9);
/// FTRAPcc when the trap isn't taken (10.7.1). The manual has no time for
/// a taken trap, which is taken as this and the exception.
pub(in crate::cpu_m68k) const FTRAPCC: Exec040 = e(1, 5);

/// Index of the destination column of MOVE for an addressing mode row
const fn move_dst_column(row: Ea040) -> usize {
    match row {
        Ea040::Dn | Ea040::An => 0,
        Ea040::Ind => 1,
        Ea040::PostInc => 2,
        Ea040::PreDec => 3,
        Ea040::Disp => 4,
        Ea040::Abs => 5,
        Ea040::Index => 6,
        Ea040::Base | Ea040::BaseDisp => 7,
        Ea040::PreIndex => 8,
        Ea040::PreIndexOd => 9,
        Ea040::PostIndex => 10,
        Ea040::PostIndexOd => 11,
        // Not valid destinations
        Ea040::PcDisp | Ea040::Imm | Ea040::PcIndex => 0,
    }
}

/// Index of the source row of MOVE for an addressing mode row
const fn move_src_row(row: Ea040) -> usize {
    match row {
        Ea040::Dn | Ea040::An => 0,
        Ea040::Ind => 1,
        Ea040::PostInc => 2,
        Ea040::PreDec => 3,
        Ea040::Disp => 4,
        Ea040::PcDisp => 5,
        Ea040::Abs => 6,
        Ea040::Imm => 7,
        Ea040::Index => 8,
        Ea040::PcIndex => 9,
        Ea040::Base | Ea040::BaseDisp => 10,
        Ea040::PreIndex => 11,
        Ea040::PreIndexOd => 12,
        Ea040::PostIndex => 13,
        Ea040::PostIndexOd => 14,
    }
}

impl<
    TBus,
    const ADDRESS_MASK: Address,
    const CPU_TYPE: CpuM68kType,
    const FPU_TYPE: FpuM68kType,
    const PMMU: bool,
> CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, FPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Row of the tables for the addressing mode `mode`, of the `nth` effective
    /// address of the instruction with a full format extension word, and one
    /// clock of lead time when the base register is the PC
    fn ea040(&self, mode: AddressingMode, nth: usize) -> (Ea040, u16) {
        let row = match mode {
            AddressingMode::DataRegister => Ea040::Dn,
            AddressingMode::AddressRegister => Ea040::An,
            AddressingMode::Indirect => Ea040::Ind,
            AddressingMode::IndirectPostInc => Ea040::PostInc,
            AddressingMode::IndirectPreDec => Ea040::PreDec,
            AddressingMode::IndirectDisplacement => Ea040::Disp,
            AddressingMode::PCDisplacement => Ea040::PcDisp,
            AddressingMode::AbsoluteShort | AddressingMode::AbsoluteLong => Ea040::Abs,
            AddressingMode::Immediate => Ea040::Imm,
            AddressingMode::IndirectIndex
            | AddressingMode::IndirectIndexBase
            | AddressingMode::PCIndex
            | AddressingMode::PCIndexBase => match self.step_ea040[nth] {
                Some(full) => return (full.row, u16::from(full.pc)),
                None if matches!(mode, AddressingMode::PCIndex | AddressingMode::PCIndexBase) => {
                    Ea040::PcIndex
                }
                None => Ea040::Index,
            },
        };
        (row, 0)
    }

    /// Time in the column `column` of the tables for the addressing mode `mode`
    /// of the only effective address of the instruction
    pub(in crate::cpu_m68k) fn exec040_ea(
        &self,
        column: &Column040,
        mode: AddressingMode,
    ) -> Exec040 {
        let (row, pc) = self.ea040(mode, 0);
        let time = column[row as usize];
        e(time.lead + pc, time.base)
    }

    /// Time of MOVE from addressing mode `src` to `dst`
    pub(in crate::cpu_m68k) fn exec040_move(
        &self,
        src: AddressingMode,
        dst: AddressingMode,
    ) -> Exec040 {
        let (src_row, pc) = self.ea040(src, 0);
        // A source with a full format extension word took the first record
        let dst_nth = usize::from(src_row as usize >= Ea040::Base as usize);
        let (dst_row, _) = self.ea040(dst, dst_nth);
        let time = MOVE[move_src_row(src_row)][move_dst_column(dst_row)];
        e(time.lead + pc, time.base)
    }

    /// Time of a bit field instruction. `spans` is None for a data register,
    /// otherwise whether the field in memory spans two long words, which the
    /// manual adds time for: taken here as its bytes crossing an aligned long
    /// word.
    pub(in crate::cpu_m68k) fn exec040_bf(
        &self,
        instr: &Instruction,
        sec: BfxExtWord,
        spans: Option<bool>,
    ) -> Result<Exec040> {
        let (column, spanning) = match instr.mnemonic {
            InstructionMnemonic::BFCHG
            | InstructionMnemonic::BFCLR
            | InstructionMnemonic::BFSET => (&BF_CHANGE, 9),
            InstructionMnemonic::BFEXTS | InstructionMnemonic::BFEXTU => (&BF_EXTRACT, 2),
            InstructionMnemonic::BFFFO => (&BFFFO, 2),
            InstructionMnemonic::BFINS => (&BFINS, 7),
            InstructionMnemonic::BFTST => (&BFTST, 0),
            _ => unreachable!(),
        };
        let time = self.exec040_ea(column, instr.get_addr_mode()?);
        Ok(match spans {
            // One more clock with the offset or width in a register
            None if sec.fdo() || sec.fdw() => time.plus(1),
            None | Some(false) => time,
            Some(true) => time.plus(spanning),
        })
    }

    /// Whether the bytes of a bit field of `width` bits at bit `offset` (0-7) of
    /// the byte at `ea` cross an aligned long word
    pub(in crate::cpu_m68k) fn bf_spans040(ea: Address, offset: i32, width: Long) -> bool {
        let last = ea.wrapping_add((offset as Long + width - 1) / 8);
        (ea ^ last) & !3 != 0
    }

    /// Time of MOVEM with the register list `mask`: one clock per register (a
    /// data register at least), and MOVEM.W to address registers one more
    /// clock for each
    pub(in crate::cpu_m68k) fn exec040_movem(
        &self,
        instr: &Instruction,
        mask: u16,
        to_regs: bool,
        word: bool,
    ) -> Result<Exec040> {
        let mode = instr.get_addr_mode()?;
        // The list of -(An) is reversed: A7 to A0, then D7 to D0
        let (data, addr) = if mode == AddressingMode::IndirectPreDec {
            ((mask >> 8).count_ones(), (mask & 0xFF).count_ones())
        } else {
            ((mask & 0xFF).count_ones(), (mask >> 8).count_ones())
        };
        let column = if to_regs {
            &MOVEM_TO_REGS
        } else {
            &MOVEM_TO_MEM
        };
        let mut regs = data.max(1) + addr;
        if to_regs && word {
            regs += addr;
        }
        Ok(self.exec040_ea(column, mode).plus(regs as u16))
    }

    /// Records the addressing mode of an effective address with a full format
    /// extension word
    pub(in crate::cpu_m68k) fn record_ea040(&mut self, full: FullEa040) {
        let nth = usize::from(self.step_ea040[0].is_some());
        self.step_ea040[nth] = Some(full);
    }

    /// Runs the instruction of this step in the execute stage, for the time
    /// the instruction set in `exec040`, less the lead time the previous
    /// instruction hides: the previous instruction executes for its base
    /// time, during which the earlier stages of this one run, all but the
    /// clock it takes to pass the operands on (MC68040UM 10.2, Figure 10-2).
    pub(in crate::cpu_m68k) fn execute040(&mut self) -> Result<()> {
        let Exec040 { lead, base } = self.exec040;
        let hidden = self.exec040_prev.saturating_sub(1);
        self.exec040_prev = base;
        self.advance_cycles(Ticks::from(base + lead.saturating_sub(hidden)))
    }
}
