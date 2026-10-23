use crate::cpu_m68k::{FPU_M68882, FpuM68kType};
use anyhow::{Result, bail};
use arpfloat::{Float, RoundingMode, Semantics};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::ops_generic::FPU_CYCLES_LEN;
use crate::cpu_m68k::fpu::trig::FloatTrig;
use crate::tickable::Ticks;

use super::{SEMANTICS_DOUBLE, SEMANTICS_EXTENDED, SEMANTICS_SINGLE};

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
    fn fpu_rounding_mode(&self) -> RoundingMode {
        // 3.5.2 Rounding modes
        // Table 3-21
        match self.regs.fpu.fpcr.rnd() {
            0b00 => RoundingMode::NearestTiesToEven,
            0b01 => RoundingMode::Zero,
            0b10 => RoundingMode::Negative,
            0b11 => RoundingMode::Positive,
            _ => unreachable!(),
        }
    }

    fn fpu_rounding_precision(&self) -> Result<Semantics> {
        // 3.5.2 Rounding modes
        // Table 3-21
        Ok(match self.regs.fpu.fpcr.prec() {
            0b00 => SEMANTICS_EXTENDED,
            0b01 => SEMANTICS_SINGLE,
            0b10 => SEMANTICS_DOUBLE,
            0b11 => bail!("Undefined rounding precision 11"),
            _ => unreachable!(),
        })
    }

    fn fpu_rounding_mode_precision(&self) -> Result<Semantics> {
        Ok(self
            .fpu_rounding_precision()?
            .with_rm(self.fpu_rounding_mode()))
    }

    pub(in crate::cpu_m68k) fn fpu_alu_op(
        &mut self,
        opmode: u8,
        source: &Float,
        dest: &Float,
    ) -> Result<(Float, [Ticks; FPU_CYCLES_LEN])> {
        debug_assert_eq!(source.get_semantics(), SEMANTICS_EXTENDED);
        debug_assert_eq!(dest.get_semantics(), SEMANTICS_EXTENDED);

        // Apply FPCR rounding mode and precision to operands
        // Operations will automatically use these semantics
        let sem = self.fpu_rounding_mode_precision()?;
        let source = &source.cast(sem);
        let dest = &dest.cast(sem);

        // Cycles row (source/destination data type):
        // [FPn to FPn, integer, single, double, extended, packed]
        //
        // There doesn't seem to be an equal difference for ALL operations
        // between the different source/destination types, therefore we just
        // list all the timings for each operation.
        let (result, cycles) = match opmode {
            // FMOVE
            0b0000000 => (source.clone(), [33, 60, 52, 58, 56, 870]),
            // FSQRT
            0b0000100 => (source.sqrt(), [107, 134, 126, 132, 130, 944]),
            // FABS
            0b0011000 => (source.abs(), [35, 62, 54, 60, 58, 872]),
            // FADD
            0b0100010 => (dest + source, [51, 80, 72, 78, 76, 888]),
            // FSUB
            0b0101000 => (dest - source, [51, 80, 72, 78, 76, 888]),
            // FMUL
            0b0100011 => (dest * source, [71, 100, 92, 98, 96, 908]),
            // FDIV
            0b0100000 => (dest / source, [103, 132, 124, 130, 128, 940]),
            // FSGLMUL - single precision with FPCR rounding mode
            0b0100111 => {
                let sem = SEMANTICS_SINGLE.with_rm(self.fpu_rounding_mode());
                let source = &source.cast(sem);
                let dest = &dest.cast(sem);
                (dest * source, [59, 88, 80, 86, 84, 896])
            }
            // FSGLDIV - single precision with FPCR rounding mode
            0b0100100 => {
                let sem = SEMANTICS_SINGLE.with_rm(self.fpu_rounding_mode());
                let source = &source.cast(sem);
                let dest = &dest.cast(sem);
                (dest / source, [69, 98, 90, 96, 94, 906])
            }
            // FINT
            0b0000001 => {
                let sem = self.fpu_rounding_mode_precision()?;
                let casted = source.cast(sem);

                // Round to integer based on rounding mode
                let rounded = match self.fpu_rounding_mode() {
                    RoundingMode::NearestTiesToEven => casted.round(),
                    RoundingMode::Zero => casted.trunc(),
                    RoundingMode::Negative => casted.floor(),
                    RoundingMode::Positive => casted.ceil(),
                    RoundingMode::None | RoundingMode::NearestTiesToAway => unreachable!(),
                };

                (rounded.cast(SEMANTICS_EXTENDED), [55, 82, 74, 80, 78, 892])
            }
            // FINTRZ
            0b0000011 => (
                source
                    .cast_with_rm(SEMANTICS_EXTENDED, arpfloat::RoundingMode::Zero)
                    .trunc()
                    .cast(SEMANTICS_EXTENDED),
                [55, 82, 74, 80, 78, 892],
            ),
            // FCMP
            0b0111000 => {
                let result = dest - source;
                self.fpu_condition_codes(&result);
                // TODO flags
                return Ok((
                    dest.cast(SEMANTICS_EXTENDED),
                    Self::fpu_cycles(opmode, [33, 62, 54, 60, 58, 870]),
                ));
            }
            // FREM - always uses round-to-nearest regardless of FPCR
            0b0100101 => {
                let sem = SEMANTICS_EXTENDED.with_rm(RoundingMode::NearestTiesToEven);
                let dest = &dest.cast(sem);
                let source = &source.cast(sem);
                let quotient = dest / source;
                let n = quotient.round();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                (dest - (source * n), [100, 129, 121, 127, 125, 937])
            }
            // FMOD - always uses round-toward-zero regardless of FPCR
            0b0100001 => {
                let sem = SEMANTICS_EXTENDED.with_rm(RoundingMode::Zero);
                let dest = &dest.cast(sem);
                let source = &source.cast(sem);
                let quotient = dest / source;
                let n = quotient.trunc();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                (dest - (source * n), [70, 99, 91, 97, 95, 907])
            }
            // FGETEXP
            0b0011110 => {
                // No need to remove the bias here as we store FPx registers unbiased
                (
                    Float::from_i64(SEMANTICS_EXTENDED, source.get_exp()),
                    [45, 72, 64, 70, 68, 882],
                )
            }
            // FTST
            0b0111010 => {
                self.fpu_condition_codes(source);
                return Ok((
                    dest.cast(SEMANTICS_EXTENDED),
                    Self::fpu_cycles(opmode, [33, 60, 52, 58, 56, 870]),
                ));
            }
            // FNEG
            0b0011010 => (source.neg(), [35, 62, 54, 60, 58, 872]),
            // FACOS
            0b0011100 => (source.acos(), [625, 652, 644, 650, 648, 1462]),
            // FCOS
            0b0011101 => (source.cos(), [391, 418, 410, 416, 414, 1228]),
            // FATAN
            0b0001010 => (source.atan(), [403, 430, 422, 428, 426, 1240]),
            // FSIN
            0b0001110 => (source.sin(), [391, 418, 410, 416, 414, 1228]),
            // FSINCOS
            0b0110000..=0b0110111 => {
                let fpc = usize::from(opmode & 0b111);

                // M6888x UM: "If FPc and FPs specify the same floating-point data register,
                // the sine result is stored in the register, and the cosine result is discarded."
                //
                // So we store the cosine result first, which will be overwritten by the sine
                // result later if destination registers are the same.
                self.regs.fpu.fp[fpc] = source.cos().cast(SEMANTICS_EXTENDED);

                (source.sin(), [451, 478, 470, 476, 474, 1288])
            }
            // FASIN
            0b0001100 => (source.asin(), [581, 608, 600, 606, 604, 1418]),
            // FTAN
            0b0001111 => (source.tan(), [473, 500, 492, 498, 496, 1310]),
            // FLOGN
            0b0010100 => (source.log(), [525, 552, 544, 550, 548, 1362]),
            // FLOGNP1
            0b0000110 => (
                (source + Float::one(source.get_semantics(), false)).log(),
                [571, 598, 590, 596, 594, 1408],
            ),
            // FLOG2
            0b0010110 => (source.log2(), [581, 608, 600, 606, 604, 1418]),
            // FLOG10
            0b0010101 => (source.log10(), [581, 608, 600, 606, 604, 1418]),
            // FETOX
            0b0010000 => (
                Float::e(SEMANTICS_EXTENDED).pow(source),
                [497, 524, 516, 522, 520, 1334],
            ),
            // FETOXM1
            0b0001000 => (
                Float::e(SEMANTICS_EXTENDED).pow(source) - 1,
                [545, 572, 564, 570, 568, 1382],
            ),
            // FTWOTOX
            0b0010001 => (
                Float::from_u64(SEMANTICS_EXTENDED, 2).pow(source),
                [567, 594, 586, 592, 590, 1404],
            ),
            // FTENTOX
            0b0010010 => (
                Float::from_u64(SEMANTICS_EXTENDED, 10).pow(source),
                [567, 594, 586, 592, 590, 1404],
            ),
            // FSINH
            0b0000010 => (source.sinh(), [687, 714, 706, 712, 710, 1524]),
            // FCOSH
            0b0011001 => (source.cosh(), [607, 634, 626, 632, 630, 1444]),
            // FTANH
            0b0001001 => (source.tanh(), [661, 688, 680, 686, 684, 1498]),
            // FATANH
            0b0001101 => (source.atanh(), [693, 720, 712, 718, 716, 1530]),
            // FSCALE
            0b0100110 => (
                dest.scale(source.trunc().to_i64(), dest.get_rounding_mode()),
                [41, 70, 62, 68, 66, 878],
            ),
            // FGETMAN
            0b0011111 => {
                (
                    if source.is_inf() || source.is_nan() {
                        // Not sure if sign gets cleared here, assuming it does
                        Float::nan(SEMANTICS_EXTENDED, false)
                    } else if source.is_zero() {
                        // Not sure if sign gets cleared here, assuming it does
                        Float::zero(SEMANTICS_EXTENDED, false)
                    } else {
                        // Decompose and recreate float to get a normalized mantissa
                        let mantissa = source.get_mantissa();
                        Float::from_parts(SEMANTICS_EXTENDED, false, 0, mantissa)
                    },
                    [31, 58, 50, 56, 54, 868],
                )
            }

            _ => bail!("Unimplemented FPU ALU op {:07b}", opmode),
        };

        // Flags
        self.regs.fpu.fpsr.exs_mut().set_bsun(false);
        self.regs.fpu.fpsr.exs_mut().set_snan(result.is_nan()); // * 1.6.5
        self.regs.fpu.fpsr.exs_mut().set_operr(false);
        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
        self.regs.fpu.fpsr.exs_mut().set_unfl(false); // * X denormalized
        self.regs.fpu.fpsr.exs_mut().set_inex2(false); // * L, D, X
        self.regs.fpu.fpsr.exs_mut().set_inex1(false); // * P
        let excs = self.regs.fpu.fpsr;
        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());

        // Condition codes (3.6.2)
        self.fpu_condition_codes(&result);

        // Cast result back to EXTENDED for storage in FPU registers
        Ok((
            result.cast(SEMANTICS_EXTENDED),
            Self::fpu_cycles(opmode, cycles),
        ))
    }

    /// Returns the execution times of an arithmetic operation, which are those of the
    /// 68881 (`m68881`) or, on a 68882, those of Table 8-3 of the MC68881/MC68882
    /// user's manual.
    fn fpu_cycles(opmode: u8, m68881: [Ticks; FPU_CYCLES_LEN]) -> [Ticks; FPU_CYCLES_LEN] {
        if FPU_TYPE != FPU_M68882 {
            return m68881;
        }

        // [FPn to FPn, integer, single, double, extended, packed]
        match opmode {
            // FMOVE
            0b0000000 => [21, 48, 34, 40, 46, 891],
            // FINT, FINTRZ
            0b0000001 | 0b0000011 => [58, 88, 71, 77, 83, 913],
            // FSINH
            0b0000010 => [690, 720, 703, 709, 715, 1545],
            // FSQRT
            0b0000100 => [110, 140, 123, 129, 135, 965],
            // FLOGNP1
            0b0000110 => [574, 604, 587, 593, 599, 1429],
            // FETOXM1
            0b0001000 => [548, 578, 561, 567, 573, 1403],
            // FTANH
            0b0001001 => [664, 694, 677, 683, 689, 1519],
            // FATAN
            0b0001010 => [406, 436, 419, 425, 431, 1261],
            // FASIN, FLOG10, FLOG2
            0b0001100 | 0b0010101 | 0b0010110 => [584, 614, 597, 603, 609, 1439],
            // FATANH
            0b0001101 => [696, 726, 709, 715, 721, 1551],
            // FSIN, FCOS
            0b0001110 | 0b0011101 => [394, 424, 407, 413, 419, 1249],
            // FTAN
            0b0001111 => [476, 506, 489, 495, 501, 1331],
            // FETOX
            0b0010000 => [500, 530, 513, 519, 525, 1355],
            // FTWOTOX, FTENTOX
            0b0010001 | 0b0010010 => [570, 600, 583, 589, 595, 1425],
            // FLOGN
            0b0010100 => [528, 558, 541, 547, 553, 1383],
            // FABS, FNEG
            0b0011000 | 0b0011010 => [38, 68, 51, 57, 63, 893],
            // FCOSH
            0b0011001 => [610, 640, 623, 629, 635, 1465],
            // FACOS
            0b0011100 => [628, 658, 641, 647, 653, 1483],
            // FGETEXP
            0b0011110 => [48, 78, 61, 67, 73, 903],
            // FGETMAN
            0b0011111 => [34, 64, 47, 53, 59, 889],
            // FDIV
            0b0100000 => [108, 146, 121, 127, 133, 961],
            // FMOD
            0b0100001 => [75, 113, 88, 94, 100, 928],
            // FADD, FSUB
            0b0100010 | 0b0101000 => [56, 94, 69, 75, 81, 909],
            // FMUL
            0b0100011 => [76, 114, 89, 95, 101, 929],
            // FSGLDIV
            0b0100100 => [74, 112, 87, 93, 99, 927],
            // FREM
            0b0100101 => [105, 143, 118, 124, 130, 958],
            // FSCALE
            0b0100110 => [46, 84, 59, 65, 71, 899],
            // FSGLMUL
            0b0100111 => [64, 102, 77, 83, 89, 917],
            // FSINCOS
            0b0110000..=0b0110111 => [454, 484, 467, 473, 479, 1309],
            // FCMP
            0b0111000 => [38, 76, 51, 57, 63, 891],
            // FTST
            0b0111010 => [36, 66, 49, 55, 61, 891],
            _ => m68881,
        }
    }

    fn fpu_condition_codes(&mut self, result: &Float) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
