use anyhow::{bail, Context, Result};
use arpfloat::Float;
use num::FromPrimitive;

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::cpu::CpuM68k;
use crate::cpu_m68k::fpu::instruction::{FmoveControlReg, FmoveExtWord};
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::instruction::Instruction;
use crate::cpu_m68k::CpuM68kType;
use crate::types::{Long, Word};

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// FNOP
    pub(in crate::cpu_m68k) fn op_fnop(&mut self, _instr: &Instruction) -> Result<()> {
        // Fetch second word (0000)
        self.fetch()?;

        Ok(())
    }

    /// FSAVE
    pub(in crate::cpu_m68k) fn op_fsave(&mut self, instr: &Instruction) -> Result<()> {
        // Idle state frame
        self.write_ea(instr, instr.get_op2(), 0x00180018u32)?;

        Ok(())
    }

    /// FRESTORE
    pub(in crate::cpu_m68k) fn op_frestore(&mut self, instr: &Instruction) -> Result<()> {
        let state = self.read_ea::<Long>(instr, instr.get_op2())?;
        if state != 0 {
            log::warn!("TODO FPU state frame restored: {:08X}", state);
        }

        Ok(())
    }

    /// FMOVE
    pub(in crate::cpu_m68k) fn op_fmove(&mut self, instr: &Instruction) -> Result<()> {
        // Fetch extension word
        let extword = FmoveExtWord(self.fetch()?);
        log::debug!("FMOVE {:04X} {:04X}", instr.data, extword.0);

        match extword.subop() {
            0b100 => {
                // From EA to control reg
                let ctrlreg = FmoveControlReg::from_u8(extword.reg())
                    .context("Invalid register selection field")?;
                let value = self.read_ea::<Long>(instr, instr.get_op2())?;
                self.regs.write(ctrlreg.into(), value);
            }
            0b101 => {
                // From control reg to EA
                let ctrlreg = FmoveControlReg::from_u8(extword.reg())
                    .context("Invalid register selection field")?;
                let value = self.regs.read::<Long>(ctrlreg.into());
                self.write_ea(instr, instr.get_op2(), value)?;
            }
            0b010 => {
                // EA to register
                let fpx = extword.dst_reg();
                let value_in = match extword.src_spec() {
                    0b100 => Float::from_u64(
                        SEMANTICS_EXTENDED,
                        self.read_ea::<Word>(instr, instr.get_op2())?.into(),
                    ),
                    _ => {
                        bail!(
                            "EA to reg unimplemented src spec {:03b}",
                            extword.src_spec()
                        );
                    }
                };

                if extword.opmode() != 0 {
                    bail!("TODO rounding precision");
                }

                // Flags
                self.regs.fpu.fpsr.exs_mut().set_bsun(false);
                self.regs.fpu.fpsr.exs_mut().set_snan(false); // * 1.6.5
                self.regs.fpu.fpsr.exs_mut().set_operr(false);
                self.regs.fpu.fpsr.exs_mut().set_ovfl(false);
                self.regs.fpu.fpsr.exs_mut().set_unfl(false); // * X denormalized
                self.regs.fpu.fpsr.exs_mut().set_inex2(false); // * L, D, X
                self.regs.fpu.fpsr.exs_mut().set_inex1(false); // * P

                // Condition codes (3.6.2)
                self.regs.fpu.fpsr.set_fpcc_nan(value_in.is_nan());
                self.regs.fpu.fpsr.set_fpcc_i(value_in.is_inf());
                self.regs.fpu.fpsr.set_fpcc_n(value_in.is_negative());
                self.regs.fpu.fpsr.set_fpcc_z(value_in.is_zero());

                log::debug!("{}", value_in);
                log::debug!("{:?}", self.regs.fpu.fpsr);
                self.regs.fpu.fp[fpx] = value_in;
            }
            _ => {
                bail!("Unimplemented sub-operation {:03b}", extword.subop());
            }
        }

        Ok(())
    }
}
