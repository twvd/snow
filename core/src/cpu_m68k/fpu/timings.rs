//! Instruction timings of the floating point units

use anyhow::Result;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::cache::CpuCaches;
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::instruction::{AddressingMode, Instruction};
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

/// Times of FMOVE and FMOVEM of the control registers
pub(in crate::cpu_m68k) enum CregTimes {
    /// The same time for every addressing mode and number of registers
    Fixed(Word),
    /// The times of MC68881/MC68882UM Table 8-6
    Mc6888x,
}

/// Instruction timings of a single FPU model
pub(in crate::cpu_m68k) struct FpuTimings {
    /// ALU operations, indexed by opmode and source operand type
    alu: [AluRow; opmode::COUNT],

    /// Part of an ALU operation the CPU waits for, by source operand type: the
    /// FPU runs the rest alongside the CPU. None if the overlap isn't modelled.
    alu_cpu: Option<AluRow>,

    /// Head of an ALU operation, by source operand type: the time it can
    /// overlap the end of the previous one. None if the FPU can't overlap two
    /// operations.
    alu_head: Option<AluRow>,

    /// FMOVE FPn to <ea>, indexed by the destination format field
    store: [Word; 8],

    /// FMOVECR
    movecr: Word,

    /// FNOP
    nop: Word,

    /// FSAVE
    save: Word,

    /// FRESTORE of a null state frame
    restore_null: Word,

    /// FRESTORE
    restore: Word,

    /// FMOVE and FMOVEM of the control registers
    move_creg: CregTimes,

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
        let opmode = match opmode {
            opmode::FSINCOS..=opmode::FSINCOS_LAST => opmode::FSINCOS,
            _ => opmode::rounded(opmode).map_or(opmode, |(op, _)| op),
        };
        self.alu[usize::from(opmode)][operand as usize].into()
    }

    /// Part of an ALU operation on the given source operand type that the CPU
    /// waits for, if the overlap is modelled
    pub(in crate::cpu_m68k) fn alu_cpu(&self, operand: FpuOperand) -> Option<Ticks> {
        self.alu_cpu.map(|row| row[operand as usize].into())
    }

    /// Head of an ALU operation on the given source operand type, 0 if the FPU
    /// can't overlap two operations
    pub(in crate::cpu_m68k) fn alu_head(&self, operand: FpuOperand) -> Ticks {
        self.alu_head.map_or(0, |row| row[operand as usize].into())
    }

    /// FMOVE FPn to <ea> with the given destination format
    pub(in crate::cpu_m68k) fn store(&self, dest_fmt: u8) -> Ticks {
        self.store[usize::from(dest_fmt)].into()
    }

    /// FMOVECR
    pub(in crate::cpu_m68k) fn movecr(&self) -> Ticks {
        self.movecr.into()
    }

    /// FNOP
    pub(in crate::cpu_m68k) fn nop(&self) -> Ticks {
        self.nop.into()
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn save(&self) -> Ticks {
        self.save.into()
    }

    /// FRESTORE of a null state frame
    pub(in crate::cpu_m68k) fn restore_null(&self) -> Ticks {
        self.restore_null.into()
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn restore(&self) -> Ticks {
        self.restore.into()
    }

    /// FMOVE and FMOVEM of `regs`, the control register field, to or from
    /// the FPU. For Table 8-6, the times of the table less the 2 instruction
    /// words, the immediate data and the operand bus cycles of 3 cycles.
    pub(in crate::cpu_m68k) fn move_creg(
        &self,
        instr: &Instruction,
        regs: u8,
        to_fpu: bool,
    ) -> Result<Ticks> {
        if let CregTimes::Fixed(time) = self.move_creg {
            return Ok(time.into());
        }
        let n = Ticks::from(regs.count_ones());
        let mode = instr.get_addr_mode()?;
        let register = matches!(
            mode,
            AddressingMode::DataRegister | AddressingMode::AddressRegister
        );
        Ok(match (n, to_fpu) {
            // FMOVE: 28 from Rn, 30 from #<data>, 33 from <ea>, 31 to Rn, 33 to <ea>
            (1, true) if register => 24,
            (1, true) if mode == AddressingMode::Immediate => 22,
            (1, false) if register => 27,
            (1, _) => 26,
            // FMOVEM: 25 + 6n from #<data>, 27 + 6n otherwise
            (_, true) if mode == AddressingMode::Immediate => 21 + 2 * n,
            _ => 23 + 3 * n,
        })
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
        (opmode::FINT,    [ 55,  92,  74,  80,  78,  892]),
        (opmode::FSINH,   [687, 714, 706, 712, 710, 1524]),
        (opmode::FINTRZ,  [ 55,  82,  74,  80,  78,  892]),
        (opmode::FSQRT,   [107, 134, 126, 132, 130,  944]),
        (opmode::FLOGNP1, [571, 598, 590, 596, 594, 1408]),
        (opmode::FETOXM1, [545, 572, 564, 570, 568, 1382]),
        (opmode::FTANH,   [661, 688, 680, 686, 684, 1498]),
        (opmode::FATAN,   [403, 430, 422, 428, 426, 1240]),
        (opmode::FASIN,   [581, 608, 600, 606, 604, 1418]),
        (opmode::FATANH,  [693, 720, 712, 718, 716, 1530]),
        (opmode::FSIN,    [391, 418, 410, 416, 414, 1228]),
        (opmode::FTAN,    [473, 500, 492, 498, 496, 1310]),
        (opmode::FETOX,   [497, 524, 516, 522, 520, 1334]),
        (opmode::FTWOTOX, [567, 594, 586, 592, 590, 1404]),
        (opmode::FTENTOX, [567, 594, 586, 592, 590, 1404]),
        (opmode::FLOGN,   [525, 552, 544, 550, 548, 1362]),
        (opmode::FLOG10,  [581, 608, 600, 606, 604, 1418]),
        (opmode::FLOG2,   [581, 608, 600, 606, 604, 1418]),
        (opmode::FABS,    [ 35,  62,  54,  60,  58,  872]),
        (opmode::FCOSH,   [607, 634, 626, 632, 630, 1444]),
        (opmode::FNEG,    [ 35,  62,  54,  60,  58,  872]),
        (opmode::FACOS,   [625, 652, 644, 650, 648, 1462]),
        (opmode::FCOS,    [391, 418, 410, 416, 414, 1228]),
        (opmode::FSINCOS, [451, 478, 470, 476, 474, 1288]),
        (opmode::FGETEXP, [ 45,  72,  64,  70,  68,  882]),
        (opmode::FGETMAN, [ 31,  58,  50,  56,  54,  868]),
        (opmode::FDIV,    [103, 132, 124, 130, 128,  940]),
        (opmode::FMOD,    [ 70,  99,  91,  97,  95,  907]),
        (opmode::FADD,    [ 51,  80,  72,  78,  76,  888]),
        (opmode::FMUL,    [ 71, 100,  92,  98,  96,  908]),
        (opmode::FSGLDIV, [ 69,  98,  90,  96,  94,  906]),
        (opmode::FREM,    [100, 129, 121, 127, 125,  937]),
        (opmode::FSCALE,  [ 41,  70,  62,  68,  66,  878]),
        (opmode::FSGLMUL, [ 59,  88,  80,  86,  84,  896]),
        (opmode::FSUB,    [ 51,  80,  72,  78,  76,  888]),
        (opmode::FCMP,    [ 33,  62,  54,  60,  58,  870]),
        (opmode::FTST,    [ 33,  60,  52,  58,  56,  870]),
    ]),

    // Table 8-25: the CPU waits 3 cycles from FPm, 22 to 34 from memory
    alu_cpu: Some([3, 22, 22, 28, 34, 34]),

    // The MC68881 can't overlap two operations
    alu_head: None,

    // long, single, extended, packed, word, double, byte, packed (dynamic K)
    store: [100, 80, 72, 2002, 100, 86, 100, 2016],

    movecr: 29,

    // Table 8-7: 18, less the instruction words
    nop: 14,

    // Table 8-8: 52 for an idle state frame, with 7 writes of 3 cycles
    save: 29,

    // Table 8-8: 21 for a null state frame and 57 for an idle one, with 1 or
    // 7 reads of 3 cycles
    restore_null: 16,
    restore: 34,

    move_creg: CregTimes::Mc6888x,

    // Table 8-6: 37 + 25n to memory and 35 + 31n from memory, with 3 writes
    // or reads of 3 cycles for each register
    fmovem_to_ea: (33, 25 - 9),
    fmovem_to_regs: (31, 31 - 9),

    // Table 8-2: "If the source or destination is an MPU data register,
    // subtract five or two clock cycles, respectively."
    dn_src: 5,
    dn_dst: 2,
};

/// MC68882 timings, Table 8-3 of the MC68881/MC68882 user's manual, for an
/// instruction run on its own: its overlap with the CPU (the head and tail
/// times of the table) is not modelled. The other times are those of the
/// MC68881.
const MC68882: FpuTimings = FpuTimings {
    #[rustfmt::skip]
    alu: alu_table(&[
        //                 FPn  int  sgl  dbl  ext  packed
        (opmode::FMOVE,   [ 21,  48,  34,  40,  46,  891]),
        (opmode::FINT,    [ 58,  88,  71,  77,  83,  913]),
        (opmode::FSINH,   [690, 720, 703, 709, 715, 1545]),
        (opmode::FINTRZ,  [ 58,  88,  71,  77,  83,  913]),
        (opmode::FSQRT,   [110, 140, 123, 129, 135,  965]),
        (opmode::FLOGNP1, [574, 604, 587, 593, 599, 1429]),
        (opmode::FETOXM1, [548, 578, 561, 567, 573, 1403]),
        (opmode::FTANH,   [664, 694, 677, 683, 689, 1519]),
        (opmode::FATAN,   [406, 436, 419, 425, 431, 1261]),
        (opmode::FASIN,   [584, 614, 597, 603, 609, 1439]),
        (opmode::FATANH,  [696, 726, 709, 715, 721, 1551]),
        (opmode::FSIN,    [394, 424, 407, 413, 419, 1249]),
        (opmode::FTAN,    [476, 506, 489, 495, 501, 1331]),
        (opmode::FETOX,   [500, 530, 513, 519, 525, 1355]),
        (opmode::FTWOTOX, [570, 600, 583, 589, 595, 1425]),
        (opmode::FTENTOX, [570, 600, 583, 589, 595, 1425]),
        (opmode::FLOGN,   [528, 558, 541, 547, 553, 1383]),
        (opmode::FLOG10,  [584, 614, 597, 603, 609, 1439]),
        (opmode::FLOG2,   [584, 614, 597, 603, 609, 1439]),
        (opmode::FABS,    [ 38,  68,  51,  57,  63,  893]),
        (opmode::FCOSH,   [610, 640, 623, 629, 635, 1465]),
        (opmode::FNEG,    [ 38,  68,  51,  57,  63,  893]),
        (opmode::FACOS,   [628, 658, 641, 647, 653, 1483]),
        (opmode::FCOS,    [394, 424, 407, 413, 419, 1249]),
        (opmode::FSINCOS, [454, 484, 467, 473, 479, 1309]),
        (opmode::FGETEXP, [ 48,  78,  61,  67,  73,  903]),
        (opmode::FGETMAN, [ 34,  64,  47,  53,  59,  889]),
        (opmode::FDIV,    [108, 146, 121, 127, 133,  961]),
        (opmode::FMOD,    [ 75, 113,  88,  94, 100,  928]),
        (opmode::FADD,    [ 56,  94,  69,  75,  81,  909]),
        (opmode::FMUL,    [ 76, 114,  89,  95, 101,  929]),
        (opmode::FSGLDIV, [ 74, 112,  87,  93,  99,  927]),
        (opmode::FREM,    [105, 143, 118, 124, 130,  958]),
        (opmode::FSCALE,  [ 46,  84,  59,  65,  71,  899]),
        (opmode::FSGLMUL, [ 64, 102,  77,  83,  89,  917]),
        (opmode::FSUB,    [ 56,  94,  69,  75,  81,  909]),
        (opmode::FCMP,    [ 38,  76,  51,  57,  63,  891]),
        (opmode::FTST,    [ 36,  66,  49,  55,  61,  891]),
    ]),

    // Table 8-3: the total less the T column
    alu_cpu: Some([21, 40, 31, 37, 43, 82]),

    // Table 8-3: the H column, about the same for every operation
    alu_head: Some([17, 21, 30, 36, 42, 13]),

    // long, single, extended, packed, word, double, byte, packed (dynamic K)
    store: [110, 38, 50, 2006, 110, 44, 110, 2020],

    movecr: 32,

    // Table 8-8: 100 for an idle state frame, with 15 writes of 3 cycles
    save: 53,

    // Table 8-8: 105 for an idle state frame, with 15 reads of 3 cycles
    restore: 58,

    ..MC68881
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
        // Not listed, assumed to be the same as FMUL and FDIV
        (opmode::FSGLMUL, [ 12,  16,  11,  11,  13,    0]),
        (opmode::FSGLDIV, [ 45,  49,  44,  44,  46,    0]),

        // Other opmodes are not executed in hardware by the 68040 but rather
        // implemented in the 68040FPSP (Table 9-10).
        //
        // For now, Snow still services them rather than hand off to the
        // FPSP but spends 0 cycles on those opmodes.
        // TODO throw unimplemented F-line with $2 frame and implement
        // 'unimplemented instruction' state frame.
    ]),

    // The overlap of the integer unit and the FPU is not modelled
    alu_cpu: None,
    alu_head: None,

    // long, single, extended, packed, word, double, byte, packed (dynamic K)
    // Packed decimal throws an unsupported data type exception on the MC68040
    store: [19, 6, 8, 0, 19, 6, 19, 0],

    // Not executed in hardware, left to the 68040FPSP like the opmodes above
    movecr: 0,
    nop: 6,
    save: 11,
    restore_null: 12,
    restore: 12,
    move_creg: CregTimes::Fixed(7),

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
static TIMINGS_68882: FpuTimings = MC68882;

/// MC68040 instruction timings
static TIMINGS_68040: FpuTimings = MC68040;

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
