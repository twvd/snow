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
    pub(in crate::cpu_m68k) fn fpu_alu_op(&mut self, opmode: u8, source: &Float) -> Result<Float> {
        let result = match opmode {
            0b000000 => {
                // FMOVE
                source.clone()
            }
            _ => bail!("Unimplemented FPU ALU op {:06b}", opmode),
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
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.is_negative());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());

        Ok(result)
    }
}
