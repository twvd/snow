use crate::cpu_m68k::FpuM68kType;
use anyhow::{Result, bail};
use arpfloat::{Float, RoundingMode, Semantics};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::cpu::{CpuError, CpuM68k};
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::trig::FloatTrig;

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
            // Undefined in the manual, it must not stop the emulator
            0b11 => SEMANTICS_EXTENDED,
            _ => unreachable!(),
        })
    }

    fn fpu_rounding_mode_precision(&self) -> Result<Semantics> {
        Ok(self
            .fpu_rounding_precision()?
            .with_rm(self.fpu_rounding_mode()))
    }

    /// FMOD and FREM: the exact remainder, and in FPSR the sign and low 7 bits
    /// of the quotient (MC68881UM FMOD, FREM)
    fn fpu_remainder(&mut self, dest: &Float, source: &Float, nearest: bool) -> Float {
        self.regs.fpu.fpsr.set_quotient(0);
        self.regs
            .fpu
            .fpsr
            .set_quotient_s(dest.is_negative() != source.is_negative());
        if dest.is_nan() || source.is_nan() || dest.is_inf() || source.is_zero() {
            return Float::nan(SEMANTICS_EXTENDED, false);
        }
        if source.is_inf() || dest.is_zero() {
            return dest.clone();
        }

        let (mut rest, mut quotient) = dest.remainder_parts(source);
        let mut negative = dest.is_negative();
        if nearest {
            let divisor = source.abs().cast(rest.get_semantics());
            let twice = rest.scale(1, RoundingMode::Zero);
            if twice > divisor || (twice == divisor && quotient & 1 != 0) {
                rest = divisor - rest;
                quotient = quotient.wrapping_add(1) & 0x7F;
                negative = !negative;
            }
        }
        self.regs.fpu.fpsr.set_quotient(quotient);
        if rest.is_zero() {
            negative = dest.is_negative();
        }
        let mut result = rest.cast(SEMANTICS_EXTENDED);
        result.set_sign(negative);
        result
    }

    /// e^x - 1 keeps the precision of small x: done in a higher precision, and
    /// below 2^-80 the result is x itself in extended precision
    fn fpu_etoxm1(x: &Float) -> Float {
        if x.is_nan() || x.is_zero() {
            return x.clone();
        }
        if x.is_inf() {
            return if x.is_negative() {
                Float::one(SEMANTICS_EXTENDED, true)
            } else {
                x.clone()
            };
        }
        if x.get_exp() < -80 {
            return x.clone();
        }
        let high = SEMANTICS_EXTENDED.increase_precision(128);
        (Float::e(high).pow(&x.cast(high)) - Float::one(high, false)).cast(SEMANTICS_EXTENDED)
    }

    /// ln(1 + x) keeps the precision of small x, the same way
    fn fpu_lognp1(x: &Float) -> Float {
        if x.is_nan() || x.is_zero() {
            return x.clone();
        }
        if x.get_exp() < -80 && !x.is_inf() {
            return x.clone();
        }
        let high = SEMANTICS_EXTENDED.increase_precision(128);
        (x.cast(high) + Float::one(high, false))
            .log()
            .cast(SEMANTICS_EXTENDED)
    }

    /// Rounds to an integer with the rounding mode of FPCR
    fn fpu_round_integer(&self, value: &Float) -> Float {
        if value.is_nan() || value.is_inf() || value.is_zero() {
            return value.clone();
        }
        let rounded = match self.fpu_rounding_mode() {
            RoundingMode::Zero => value.trunc(),
            RoundingMode::Negative => value.floor(),
            RoundingMode::Positive => value.ceil(),
            _ => {
                let trunc = value.trunc();
                let one = Float::one(trunc.get_semantics(), value.is_negative());
                let half = Float::one(trunc.get_semantics(), false).scale(-1, RoundingMode::Zero);
                let rest = (value - &trunc).abs();
                let odd = (&trunc / Float::from_u64(trunc.get_semantics(), 2)).trunc()
                    * Float::from_u64(trunc.get_semantics(), 2)
                    != trunc;
                if rest > half || (rest == half && odd) {
                    trunc + one
                } else {
                    trunc
                }
            }
        };
        // A zero keeps the sign of what it comes from
        if rounded.is_zero() {
            Float::zero(rounded.get_semantics(), value.is_negative())
        } else {
            rounded
        }
    }

    /// FMOVE of FPn to an integer between `min` and `max`: rounds with the
    /// rounding mode of FPCR. Inexact sets INEX2. Not fitting sets OPERR and gives
    /// the largest integer of that sign (MC68881UM 6.1.3).
    pub(in crate::cpu_m68k) fn fpu_to_integer(&mut self, fpx: usize, min: i64, max: i64) -> i64 {
        let value = self.regs.fpu.fp[fpx].clone();
        self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
        self.regs.fpu.fpsr.exs_mut().set_unfl(false);
        self.regs.fpu.fpsr.exs_mut().set_inex1(false);
        self.regs.fpu.fpsr.exs_mut().set_inex2(false);

        let rounded = self.fpu_round_integer(&value);
        let sem = rounded.get_semantics();
        let out = if value.is_nan() || rounded > Float::from_i64(sem, max) {
            self.regs.fpu.fpsr.exs_mut().set_operr(true);
            max
        } else if rounded < Float::from_i64(sem, min) {
            self.regs.fpu.fpsr.exs_mut().set_operr(true);
            min
        } else {
            self.regs.fpu.fpsr.exs_mut().set_inex2(rounded != value);
            rounded.to_i64()
        };
        let excs = self.regs.fpu.fpsr;
        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
        out
    }

    pub(in crate::cpu_m68k) fn fpu_alu_op(
        &mut self,
        opmode: u8,
        source: &Float,
        dest: &Float,
    ) -> Result<Float> {
        debug_assert_eq!(source.get_semantics(), SEMANTICS_EXTENDED);
        debug_assert_eq!(dest.get_semantics(), SEMANTICS_EXTENDED);

        // The operands are used as they are, only the result gets the rounding
        // precision of FPCR (MC68881UM 3.5.2). Operations work in extended
        // precision with the rounding mode of FPCR.
        let sem = self.fpu_rounding_mode_precision()?;
        let ext = SEMANTICS_EXTENDED.with_rm(self.fpu_rounding_mode());
        let source = &source.cast(ext);
        let dest = &dest.cast(ext);

        // Each arithmetic instruction starts its exception status anew
        self.regs.fpu.fpsr.exs_mut().0 = 0;

        let result = match opmode {
            opmode::FMOVE => source.clone(),
            opmode::FSQRT => source.sqrt(),
            opmode::FABS => source.abs(),
            opmode::FADD => dest + source,
            opmode::FSUB => dest - source,
            opmode::FMUL => dest * source,
            opmode::FDIV => dest / source,
            // Single precision with FPCR rounding mode
            opmode::FSGLMUL => {
                let sem = SEMANTICS_SINGLE.with_rm(self.fpu_rounding_mode());
                let source = &source.cast(sem);
                let dest = &dest.cast(sem);
                dest * source
            }
            // Single precision with FPCR rounding mode
            opmode::FSGLDIV => {
                let sem = SEMANTICS_SINGLE.with_rm(self.fpu_rounding_mode());
                let source = &source.cast(sem);
                let dest = &dest.cast(sem);
                dest / source
            }
            opmode::FINT => self.fpu_round_integer(source),
            opmode::FINTRZ => source
                .cast_with_rm(SEMANTICS_EXTENDED, arpfloat::RoundingMode::Zero)
                .trunc()
                .cast(SEMANTICS_EXTENDED),
            opmode::FCMP => {
                // Equal infinities and zeros compare as equal, with the sign of
                // the destination (MC68881UM FCMP)
                let result = if (dest.is_inf() || dest.is_zero())
                    && (source.is_inf() || source.is_zero())
                    && (dest.is_inf() == source.is_inf())
                    && (dest.is_zero() || dest.is_negative() == source.is_negative())
                {
                    Float::zero(ext, dest.is_negative())
                } else {
                    dest - source
                };
                self.fpu_condition_codes(&result);
                // TODO flags
                return Ok(dest.cast(SEMANTICS_EXTENDED));
            }
            // Round to nearest whatever FPCR says
            opmode::FREM => self.fpu_remainder(dest, source, true),
            // Round to zero whatever FPCR says
            opmode::FMOD => self.fpu_remainder(dest, source, false),
            opmode::FGETEXP => {
                if source.is_inf() || source.is_nan() {
                    Float::nan(SEMANTICS_EXTENDED, false)
                } else if source.is_zero() {
                    source.clone()
                } else {
                    // No need to remove the bias here as we store FPx registers unbiased
                    Float::from_i64(SEMANTICS_EXTENDED, source.get_exp())
                }
            }
            opmode::FTST => {
                self.fpu_condition_codes(source);
                return Ok(dest.cast(SEMANTICS_EXTENDED));
            }
            opmode::FNEG => source.neg(),
            opmode::FACOS => source.acos(),
            opmode::FCOS => source.cos(),
            opmode::FATAN => source.atan(),
            opmode::FSIN => source.sin(),
            opmode::FSINCOS..=opmode::FSINCOS_LAST => {
                let fpc = usize::from(opmode & 0b111);

                // M6888x UM: "If FPc and FPs specify the same floating-point data register,
                // the sine result is stored in the register, and the cosine result is discarded."
                //
                // So we store the cosine result first, which will be overwritten by the sine
                // result later if destination registers are the same.
                self.regs.fpu.fp[fpc] = source.cos().cast(SEMANTICS_EXTENDED);

                source.sin()
            }
            opmode::FASIN => source.asin(),
            opmode::FTAN => source.tan(),
            opmode::FLOGN => source.log(),
            opmode::FLOGNP1 => Self::fpu_lognp1(source),
            opmode::FLOG2 => source.log2(),
            opmode::FLOG10 => source.log10(),
            opmode::FETOX => Float::e(SEMANTICS_EXTENDED).pow(source),
            opmode::FETOXM1 => Self::fpu_etoxm1(source),
            opmode::FTWOTOX => Float::from_u64(SEMANTICS_EXTENDED, 2).pow(source),
            opmode::FTENTOX => Float::from_u64(SEMANTICS_EXTENDED, 10).pow(source),
            opmode::FSINH => source.sinh(),
            opmode::FCOSH => source.cosh(),
            opmode::FTANH => source.tanh(),
            opmode::FATANH => source.atanh(),
            opmode::FSCALE => dest.scale(source.trunc().to_i64(), dest.get_rounding_mode()),
            opmode::FGETMAN => {
                if source.is_inf() || source.is_nan() {
                    Float::nan(SEMANTICS_EXTENDED, false)
                } else if source.is_zero() {
                    source.clone()
                } else {
                    // Decompose and recreate float to get a normalized mantissa,
                    // with the sign of the source (MC68881UM FGETMAN)
                    Float::from_parts(
                        SEMANTICS_EXTENDED,
                        source.is_negative(),
                        0,
                        source.get_mantissa(),
                    )
                }
            }

            _ => bail!(CpuError::IllegalInstruction),
        };

        let result = result.cast(sem);
        self.fpu_set_status(opmode, source, dest, &result);

        // Condition codes (3.6.2)
        self.fpu_condition_codes(&result);

        // Cast result back to EXTENDED for storage in FPU registers
        Ok(result.cast(SEMANTICS_EXTENDED))
    }

    /// Sets the exception status of an arithmetic instruction and accrues it
    /// (MC68881UM 6.1). Snow's registers don't keep the payload of a NaN, so a
    /// signalling NaN can't be told apart and SNAN is never set.
    fn fpu_set_status(&mut self, opmode: u8, source: &Float, dest: &Float, result: &Float) {
        let dyadic = matches!(opmode, 0b0100000..=0b0101000);
        let inputs_nan = source.is_nan() || (dyadic && dest.is_nan());
        let inputs_inf = source.is_inf() || (dyadic && dest.is_inf());
        let divide = matches!(opmode, 0b0100000 | 0b0100100);
        let pole = matches!(
            opmode,
            0b0010100 | 0b0010101 | 0b0010110 | 0b0000110 | 0b0001101
        );
        let dz = result.is_inf() && !inputs_inf && (divide || pole);
        let ovfl = result.is_inf() && !inputs_inf && !dz;
        let unfl = result.is_zero()
            && matches!(
                opmode,
                0b0100011 | 0b0100000 | 0b0100110 | 0b0100111 | 0b0100100
            )
            && !inputs_inf
            && !source.is_zero()
            && !dest.is_zero();

        let inex2 = ovfl || unfl || Self::fpu_inexact(opmode, source, dest, result);
        let fpsr = &mut self.regs.fpu.fpsr;
        fpsr.exs_mut().set_operr(result.is_nan() && !inputs_nan);
        fpsr.exs_mut().set_dz(dz);
        fpsr.exs_mut().set_ovfl(ovfl);
        fpsr.exs_mut().set_unfl(unfl);
        fpsr.exs_mut().set_inex2(inex2);

        let excs = self.regs.fpu.fpsr;
        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
    }

    /// Tells if `result` is not the exact result of the instruction: compares
    /// with the operation in a much higher precision for the basic operations,
    /// and takes the others to be inexact unless the result is not a number.
    fn fpu_inexact(opmode: u8, source: &Float, dest: &Float, result: &Float) -> bool {
        if result.is_nan() || result.is_inf() {
            return false;
        }
        let high = SEMANTICS_EXTENDED.increase_precision(256);
        let (s, d) = (source.cast(high), dest.cast(high));
        let exact = match opmode {
            0b0000000 | 0b0011000 | 0b0011010 => s.abs(),
            0b0100010 => return (&d + &s).abs() != result.cast(high).abs(),
            0b0101000 => return (&d - &s).abs() != result.cast(high).abs(),
            0b0100011 => (&d * &s).abs(),
            0b0100000 if !s.is_zero() => (&d / &s).abs(),
            0b0000100 => s.sqrt().abs(),
            // Whole numbers, and FMOD and FREM are exact
            0b0000001 | 0b0000011 => return result.cast(high) != s,
            0b0100101 | 0b0100001 | 0b0011110 | 0b0011111 | 0b0100110 => return false,
            _ => return !result.is_zero(),
        };
        result.cast(high).abs() != exact
    }

    pub(in crate::cpu_m68k) fn fpu_condition_codes(&mut self, result: &Float) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
