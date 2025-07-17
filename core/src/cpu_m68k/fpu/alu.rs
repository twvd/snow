use anyhow::{bail, Result};
use arpfloat::{Float, RoundingMode, Semantics};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::trig::FloatTrig;
use crate::cpu_m68k::CpuM68kType;

use super::{SEMANTICS_DOUBLE, SEMANTICS_EXTENDED, SEMANTICS_SINGLE};

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE>
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

        let result = match opmode {
            // FMOVE
            0b0000000 => source.clone(),
            // FSQRT
            0b0000100 => source.sqrt(),
            // FABS
            0b0011000 => source.abs(),
            // FADD
            0b0100010 => dest + source,
            // FSUB
            0b0101000 => dest - source,
            // FMUL
            0b0100011 => dest * source,
            // FDIV
            0b0100000 => dest / source,
            // FINT
            0b0000001 => source
                .cast(self.fpu_rounding_mode_precision()?)
                .round()
                .cast(SEMANTICS_EXTENDED),
            // FINTRZ
            0b0000011 => source
                .cast_with_rm(SEMANTICS_EXTENDED, arpfloat::RoundingMode::Zero)
                .round()
                .cast(SEMANTICS_EXTENDED),
            // FCMP
            0b0111000 => {
                let result = dest - source;
                self.fpu_condition_codes(&result);
                // TODO flags
                return Ok(dest.clone());
            }
            // FREM
            0b0100101 => {
                assert_eq!(dest.get_rounding_mode(), RoundingMode::NearestTiesToEven);
                assert_eq!(source.get_rounding_mode(), RoundingMode::NearestTiesToEven);
                let quotient = dest / source;
                let n = quotient.round();
                self.regs.fpu.fpsr.set_quotient(n.to_i64() as u8);
                self.regs.fpu.fpsr.set_quotient_s(n.is_negative());
                dest - (source * n)
            }
            // FGETEXP
            0b0011110 => {
                // No need to remove the bias here as we store FPx registers unbiased
                Float::from_i64(SEMANTICS_EXTENDED, source.get_exp())
            }
            // FTST
            0b0111010 => {
                self.fpu_condition_codes(source);
                return Ok(dest.clone());
            }
            // FNEG
            0b0011010 => source.neg(),
            // FCOS
            0b0011101 => source.cos(),
            // FATAN
            0b0001010 => source.atan(),
            // FSIN
            0b0001110 => source.sin(),
            // FTAN
            0b0001111 => source.tan(),
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

        Ok(result)
    }

    fn fpu_condition_codes(&mut self, result: &Float) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
