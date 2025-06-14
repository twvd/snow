use anyhow::{bail, Result};
use arpfloat::Float;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::CpuM68kType;

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    pub(in crate::cpu_m68k) fn fpu_alu_op(
        &mut self,
        opmode: u8,
        source: &Float,
        dest: &Float,
    ) -> Result<Float> {
        let result = match opmode {
            // FMOVE
            0b0000000 => source.clone(),
            // FABS
            0b0011000 => source.abs(),
            // FMUL
            0b0100011 => source * dest,
            // FINT
            // TODO rounding mode
            0b0000001 => source.round(),
            // FCMP
            0b0111000 => {
                let result = dest - source;
                self.fpu_condition_codes(&result);
                // TODO flags
                return Ok(dest.clone());
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

        Ok(result)
    }

    fn fpu_condition_codes(&mut self, result: &Float) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
