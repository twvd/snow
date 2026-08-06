use crate::cpu_m68k::FpuM68kType;
use anyhow::{Result, bail};
use arpfloat::{Float, RoundingMode, Semantics};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::cpu::CpuM68k;
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
    ) -> Result<Float> {
        debug_assert_eq!(source.get_semantics(), SEMANTICS_EXTENDED);
        debug_assert_eq!(dest.get_semantics(), SEMANTICS_EXTENDED);

        // Apply FPCR rounding mode and precision to operands
        // Operations will automatically use these semantics
        let sem = self.fpu_rounding_mode_precision()?;
        let source = &source.cast(sem);
        let dest = &dest.cast(sem);

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
            opmode::FINT => {
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

                rounded.cast(SEMANTICS_EXTENDED)
            }
            opmode::FINTRZ => source
                .cast_with_rm(SEMANTICS_EXTENDED, arpfloat::RoundingMode::Zero)
                .trunc()
                .cast(SEMANTICS_EXTENDED),
            opmode::FCMP => {
                let result = dest - source;
                self.fpu_condition_codes(&result);
                // TODO flags
                return Ok(dest.cast(SEMANTICS_EXTENDED));
            }
            // Always uses round-to-nearest regardless of FPCR
            opmode::FREM => {
                let sem = SEMANTICS_EXTENDED.with_rm(RoundingMode::NearestTiesToEven);
                let dest = &dest.cast(sem);
                let source = &source.cast(sem);
                let quotient = dest / source;
                let n = quotient.round();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                dest - (source * n)
            }
            // Always uses round-toward-zero regardless of FPCR
            opmode::FMOD => {
                let sem = SEMANTICS_EXTENDED.with_rm(RoundingMode::Zero);
                let dest = &dest.cast(sem);
                let source = &source.cast(sem);
                let quotient = dest / source;
                let n = quotient.trunc();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                dest - (source * n)
            }
            opmode::FGETEXP => {
                // No need to remove the bias here as we store FPx registers unbiased
                Float::from_i64(SEMANTICS_EXTENDED, source.get_exp())
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
            opmode::FASIN => source.asin(),
            opmode::FTAN => source.tan(),
            opmode::FLOGN => source.log(),
            opmode::FLOGNP1 => (source + Float::one(source.get_semantics(), false)).log(),
            opmode::FLOG2 => source.log2(),
            opmode::FLOG10 => source.log10(),
            opmode::FETOX => Float::e(SEMANTICS_EXTENDED).pow(source),
            opmode::FETOXM1 => Float::e(SEMANTICS_EXTENDED).pow(source) - 1,
            opmode::FTWOTOX => Float::from_u64(SEMANTICS_EXTENDED, 2).pow(source),
            opmode::FTENTOX => Float::from_u64(SEMANTICS_EXTENDED, 10).pow(source),
            opmode::FSINH => source.sinh(),
            opmode::FCOSH => source.cosh(),
            opmode::FTANH => source.tanh(),
            opmode::FATANH => source.atanh(),
            opmode::FSCALE => dest.scale(source.trunc().to_i64(), dest.get_rounding_mode()),
            opmode::FGETMAN => {
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
                }
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
        Ok(result.cast(SEMANTICS_EXTENDED))
    }

    fn fpu_condition_codes(&mut self, result: &Float) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
