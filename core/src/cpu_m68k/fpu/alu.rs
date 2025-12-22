use crate::cpu_m68k::FpuM68kType;
use anyhow::{bail, Result};
use arpfloat::{Float, RoundingMode, Semantics};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::ops_generic::FPU_CYCLES_LEN;
use crate::cpu_m68k::fpu::trig::FloatTrig;
use crate::cpu_m68k::CpuM68kType;
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
            0b0000100 => (source.sqrt(), [107, 134, 126, 132, 130, 844]),
            // FABS
            0b0011000 => (source.abs(), [35, 62, 54, 60, 58, 872]),
            // FADD
            0b0100010 => (dest + source, [51, 80, 72, 78, 76, 888]),
            // FSUB
            0b0101000 => (dest - source, [51, 80, 72, 78, 76, 888]),
            // FMUL
            0b0100011 => (dest * source, [71, 100, 92, 98, 96, 895]),
            // FDIV
            0b0100000 => (dest / source, [105, 132, 124, 130, 128, 940]),
            // FSGLMUL
            0b0100111 => (
                (dest * source)
                    .cast(SEMANTICS_SINGLE)
                    .cast(SEMANTICS_EXTENDED),
                [59, 88, 80, 86, 84, 895],
            ),
            // FSGLDIV
            0b0100100 => (
                (dest / source)
                    .cast(SEMANTICS_SINGLE)
                    .cast(SEMANTICS_EXTENDED),
                [69, 98, 90, 96, 94, 936],
            ),
            // FINT
            0b0000001 => (
                source
                    .cast(self.fpu_rounding_mode_precision()?)
                    .round()
                    .cast(SEMANTICS_EXTENDED),
                [65, 92, 74, 80, 78, 892],
            ),
            // FINTRZ
            0b0000011 => (
                source
                    .cast_with_rm(SEMANTICS_EXTENDED, arpfloat::RoundingMode::Zero)
                    .round()
                    .cast(SEMANTICS_EXTENDED),
                [55, 82, 74, 80, 78, 892],
            ),
            // FCMP
            0b0111000 => {
                let result = dest - source;
                self.fpu_condition_codes(&result);
                // TODO flags
                return Ok((dest.clone(), [35, 62, 54, 60, 58, 870]));
            }
            // FREM
            0b0100101 => {
                assert_eq!(dest.get_rounding_mode(), RoundingMode::NearestTiesToEven);
                assert_eq!(source.get_rounding_mode(), RoundingMode::NearestTiesToEven);
                let quotient = dest / source;
                let n = quotient.round();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                (dest - (source * n), [100, 129, 121, 127, 125, 937])
            }
            // FMOD
            0b0100001 => {
                assert_eq!(dest.get_rounding_mode(), RoundingMode::NearestTiesToEven);
                assert_eq!(source.get_rounding_mode(), RoundingMode::NearestTiesToEven);
                let quotient = dest / source;
                let n = quotient.trunc();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                (dest - (source * n), [80, 99, 91, 97, 95, 907])
            }
            // FGETEXP
            0b0011110 => {
                // No need to remove the bias here as we store FPx registers unbiased
                (
                    Float::from_i64(SEMANTICS_EXTENDED, source.get_exp()),
                    [35, 72, 64, 70, 68, 882],
                )
            }
            // FTST
            0b0111010 => {
                self.fpu_condition_codes(source);
                return Ok((dest.clone(), [33, 60, 52, 58, 56, 870]));
            }
            // FNEG
            0b0011010 => (source.neg(), [35, 62, 54, 60, 58, 872]),
            // FCOS
            0b0011101 => (source.cos(), [391, 418, 410, 416, 414, 1228]),
            // FATAN
            0b0001010 => (source.atan(), [403, 430, 422, 428, 426, 1240]),
            // FSIN
            0b0001110 => (source.sin(), [391, 418, 410, 416, 414, 1228]),
            // FTAN
            0b0001111 => (source.tan(), [473, 500, 492, 498, 495, 1310]),
            // FLOGN
            0b0010100 => (source.log(), [525, 552, 544, 550, 548, 1352]),
            // FLOGNP1
            0b0000110 => (
                (source + Float::one(source.get_semantics(), false)).log(),
                [571, 598, 590, 596, 594, 1428],
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
            0b0001001 => (source.tanh(), [661, 688, 680, 686, 684, 1439]),
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
                    [31, 58, 50, 56, 54, 858],
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

        // Condition codes (3.6.2)
        self.fpu_condition_codes(&result);

        Ok((result, cycles))
    }

    fn fpu_condition_codes(&mut self, result: &Float) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
