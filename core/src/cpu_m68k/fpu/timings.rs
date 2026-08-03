//! Instruction timings of the floating point units

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::{CpuM68kType, FPU_M68040, FPU_M68881, FPU_M68882, FpuM68kType};
use crate::tickable::Ticks;
use crate::types::Word;

/// Where the source operand of an ALU operation comes from
#[derive(Debug, Clone, Copy)]
pub(in crate::cpu_m68k) enum FpuOperand {
    /// Another FPU register
    Fpn = 0,
    /// Integer (byte, word or long)
    Int,
    /// Single precision real
    Single,
    /// Double precision real
    Double,
    /// Extended precision real
    Extended,
    /// Packed BCD decimal real
    Packed,
}

/// Amount of source operand types
const OPERANDS: usize = 6;

/// One row of ALU timings, indexed by FpuOperand
type AluRow = [Word; OPERANDS];

/// Instruction timings of a single FPU model
pub(in crate::cpu_m68k) struct FpuTimings {
    /// ALU operations, indexed by opmode and source operand type
    alu: [AluRow; opmode::COUNT],

    /// FMOVE FPn to <ea>, indexed by the destination format field
    store: [Word; 8],

    /// FNOP
    nop: Word,

    /// FSAVE
    save: Word,

    /// FRESTORE
    restore: Word,

    /// FMOVE to/from a control register
    move_creg: Word,

    /// FMOVEM to <ea>: fixed cost and cost per register, excluding the bus
    /// accesses of the transfer itself
    fmovem_to_ea: (Word, Word),

    /// FMOVEM to registers: fixed cost and cost per register, excluding the bus
    /// accesses of the transfer itself
    fmovem_to_regs: (Word, Word),

    /// Discount when the source operand is an MPU data register
    dn_src: Word,

    /// Discount when the destination is an MPU data register
    dn_dst: Word,
}

impl FpuTimings {
    /// ALU operation with the given opmode and source operand type
    pub(in crate::cpu_m68k) fn alu(&self, opmode: u8, operand: FpuOperand) -> Ticks {
        self.alu[usize::from(opmode)][operand as usize].into()
    }

    /// FMOVE FPn to <ea> with the given destination format
    pub(in crate::cpu_m68k) fn store(&self, dest_fmt: u8) -> Ticks {
        self.store[usize::from(dest_fmt)].into()
    }

    /// FNOP
    pub(in crate::cpu_m68k) fn nop(&self) -> Ticks {
        self.nop.into()
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn save(&self) -> Ticks {
        self.save.into()
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn restore(&self) -> Ticks {
        self.restore.into()
    }

    /// FMOVE to/from a control register
    pub(in crate::cpu_m68k) fn move_creg(&self) -> Ticks {
        self.move_creg.into()
    }

    /// FMOVEM to <ea>: fixed cost and cost per register, excluding the bus
    /// accesses of the transfer itself
    pub(in crate::cpu_m68k) fn fmovem_to_ea(&self) -> (Ticks, Ticks) {
        (self.fmovem_to_ea.0.into(), self.fmovem_to_ea.1.into())
    }

    /// FMOVEM to registers: fixed cost and cost per register, excluding the bus
    /// accesses of the transfer itself
    pub(in crate::cpu_m68k) fn fmovem_to_regs(&self) -> (Ticks, Ticks) {
        (self.fmovem_to_regs.0.into(), self.fmovem_to_regs.1.into())
    }

    /// Discount when the source operand is an MPU data register
    pub(in crate::cpu_m68k) fn dn_src(&self) -> Ticks {
        self.dn_src.into()
    }

    /// Discount when the destination is an MPU data register
    pub(in crate::cpu_m68k) fn dn_dst(&self) -> Ticks {
        self.dn_dst.into()
    }
}

const fn alu_table(rows: &[(u8, AluRow)]) -> [AluRow; opmode::COUNT] {
    let mut table = [[0; OPERANDS]; opmode::COUNT];
    let mut i = 0;
    while i < rows.len() {
        table[rows[i].0 as usize] = rows[i].1;
        i += 1;
    }
    table
}

/// MC68881 timings, Table 8-1 and 8-2 of the MC68881 user's manual
const MC68881: FpuTimings = FpuTimings {
    #[rustfmt::skip]
    alu: alu_table(&[
        //                 FPn  int  sgl  dbl  ext  packed
        (opmode::FMOVE,   [ 33,  60,  52,  58,  56,  870]),
        (opmode::FINT,    [ 65,  92,  74,  80,  78,  892]),
        (opmode::FSINH,   [687, 714, 706, 712, 710, 1524]),
        (opmode::FINTRZ,  [ 55,  82,  74,  80,  78,  892]),
        (opmode::FSQRT,   [107, 134, 126, 132, 130,  844]),
        (opmode::FLOGNP1, [571, 598, 590, 596, 594, 1428]),
        (opmode::FETOXM1, [545, 572, 564, 570, 568, 1382]),
        (opmode::FTANH,   [661, 688, 680, 686, 684, 1439]),
        (opmode::FATAN,   [403, 430, 422, 428, 426, 1240]),
        (opmode::FASIN,   [581, 608, 600, 606, 604, 1418]),
        (opmode::FATANH,  [693, 720, 712, 718, 716, 1530]),
        (opmode::FSIN,    [391, 418, 410, 416, 414, 1228]),
        (opmode::FTAN,    [473, 500, 492, 498, 495, 1310]),
        (opmode::FETOX,   [497, 524, 516, 522, 520, 1334]),
        (opmode::FTWOTOX, [567, 594, 586, 592, 590, 1404]),
        (opmode::FTENTOX, [567, 594, 586, 592, 590, 1404]),
        (opmode::FLOGN,   [525, 552, 544, 550, 548, 1352]),
        (opmode::FLOG10,  [581, 608, 600, 606, 604, 1418]),
        (opmode::FLOG2,   [581, 608, 600, 606, 604, 1418]),
        (opmode::FABS,    [ 35,  62,  54,  60,  58,  872]),
        (opmode::FCOSH,   [607, 634, 626, 632, 630, 1444]),
        (opmode::FNEG,    [ 35,  62,  54,  60,  58,  872]),
        (opmode::FACOS,   [625, 652, 644, 650, 648, 1462]),
        (opmode::FCOS,    [391, 418, 410, 416, 414, 1228]),
        (opmode::FGETEXP, [ 35,  72,  64,  70,  68,  882]),
        (opmode::FGETMAN, [ 31,  58,  50,  56,  54,  858]),
        (opmode::FDIV,    [105, 132, 124, 130, 128,  940]),
        (opmode::FMOD,    [ 80,  99,  91,  97,  95,  907]),
        (opmode::FADD,    [ 51,  80,  72,  78,  76,  888]),
        (opmode::FMUL,    [ 71, 100,  92,  98,  96,  895]),
        (opmode::FSGLDIV, [ 69,  98,  90,  96,  94,  936]),
        (opmode::FREM,    [100, 129, 121, 127, 125,  937]),
        (opmode::FSCALE,  [ 41,  70,  62,  68,  66,  878]),
        (opmode::FSGLMUL, [ 59,  88,  80,  86,  84,  895]),
        (opmode::FSUB,    [ 51,  80,  72,  78,  76,  888]),
        (opmode::FCMP,    [ 35,  62,  54,  60,  58,  870]),
        (opmode::FTST,    [ 33,  60,  52,  58,  56,  870]),
    ]),

    // long, single, extended, packed, word, double, byte, packed (dynamic K)
    // TODO the timings of both packed formats are guesses
    store: [100, 80, 72, 80, 100, 86, 100, 80],

    nop: 16,
    save: 50,
    restore: 55,
    move_creg: 29,

    // Table 8-2 lists 25 and 31 per register, including the three long word
    // transfers of the extended precision operand
    fmovem_to_ea: (35, 25 - 12),
    fmovem_to_regs: (33, 31 - 12),

    // Table 8-2: "If the source or destination is an MPU data register,
    // subtract five or two clock cycles, respectively."
    dn_src: 5,
    dn_dst: 2,
};

/// MC68040 timings, section 10.7 of the M68040 user's manual
///
/// The MC68040 splits the cost of a floating point instruction over two
/// pipelines: the integer unit calculates the effective address and moves the
/// operands (10.7.1 and 10.7.2) and the floating point unit converts, executes
/// and normalizes (10.7.3). The timings below are the sum of both for an isolated
/// instruction on an idle FPU: the integer unit execute time for the (An) addressing
/// mode plus the three floating point stages for normalized operands.
///
/// The integer unit support for an operand is 2 cycles, except for extended
/// precision and another FPU register, which are 3.
const MC68040: FpuTimings = FpuTimings {
    #[rustfmt::skip]
    alu: alu_table(&[
        // Operations the MC68040 implements in hardware.
        //
        // 10.7.3, conversion + execution + normalization for normalized operands.
        // Packed decimal operands are not supported by the hardware.
        //                 FPn  int  sgl  dbl  ext  packed
        (opmode::FMOVE,   [  5,  10,   5,   5,   7,    0]),
        (opmode::FABS,    [  5,  10,   5,   5,   7,    0]),
        (opmode::FNEG,    [  5,  10,   5,   5,   7,    0]),
        (opmode::FADD,    [ 10,  14,   9,   9,  11,    0]),
        (opmode::FSUB,    [ 10,  14,   9,   9,  11,    0]),
        (opmode::FMUL,    [ 12,  16,  11,  11,  13,    0]),
        (opmode::FDIV,    [ 45,  49,  44,  44,  46,    0]),
        (opmode::FSQRT,   [110, 114, 109, 109, 111,    0]),
        (opmode::FCMP,    [  9,  13,   8,   8,  10,    0]),
        // Not listed, assumed to be the same as FABS/FNEG
        (opmode::FTST,    [  5,  10,   5,   5,   7,    0]),

        // Other opmodes are not executed in hardware by the 68040 but rather
        // implemented in the 68040FPSP.
    ]),

    // long, single, extended, packed, word, double, byte, packed (dynamic K)
    // Packed decimal throws an unsupported data type exception on the MC68040
    store: [19, 6, 8, 0, 19, 6, 19, 0],

    nop: 6,
    save: 11,
    restore: 12,
    move_creg: 7,

    // 15 cycles in the integer unit and 2 + 3 per register in the FPU, of which
    // 3 + 3 per register is for the second and further registers
    fmovem_to_ea: (14, 6),
    fmovem_to_regs: (14, 6),

    // The MC68040 has no discount for MPU data registers
    dn_src: 0,
    dn_dst: 0,
};

/// MC68881 instruction timings
static TIMINGS_68881: FpuTimings = MC68881;

/// MC68882 instruction timings
/// TODO use actual 68882 timings
static TIMINGS_68882: FpuTimings = MC68881;

/// MC68040 instruction timings
static TIMINGS_68040: FpuTimings = MC68040;

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
    /// Instruction timings of the FPU this CPU is configured with
    #[inline(always)]
    pub(in crate::cpu_m68k) fn fpu_timings() -> &'static FpuTimings {
        match FPU_TYPE {
            FPU_M68881 => &TIMINGS_68881,
            FPU_M68882 => &TIMINGS_68882,
            FPU_M68040 => &TIMINGS_68040,
            _ => unreachable!(),
        }
    }
}
