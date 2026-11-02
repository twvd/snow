use crate::cpu_m68k::cache::CpuCaches;
use crate::cpu_m68k::{FPU_M68040, FPU_M68881, FPU_M68882, FpuM68kType};
use anyhow::{Result, bail};
use arpfloat::{Float, RoundingMode};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::cpu::{
    CpuError, CpuM68k, ExceptionGroup, VECTOR_FP_BSUN, VECTOR_FP_DZ, VECTOR_FP_INEX,
    VECTOR_FP_OPERR, VECTOR_FP_OVFL, VECTOR_FP_SNAN, VECTOR_FP_UNFL,
};
use crate::cpu_m68k::fpu::instruction::{FmoveExtWord, opmode};
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::regs::{Busy040, Unimp040};
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::cpu_m68k::fpu::trig::FloatTrig;
use crate::types::Long;

/// The bits of OVFL and UNFL in the exception byte: on the MC68040 they always take their
/// exception, as the M68040FPSP gives the result (MC68040UM 9.7.4.2, 9.7.5.2)
pub(in crate::cpu_m68k) const NONMASKABLE_040: Long = (1 << 4) | (1 << 3);
const EXC_SNAN: Long = 1 << 6;

use super::arith::{self, Range, Unpacked, Value};
use super::constants;
use super::{SEMANTICS_EXTENDED, SEMANTICS_FUNCTIONS};

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
    pub(in crate::cpu_m68k) fn fpu_rounding_mode(&self) -> RoundingMode {
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

    /// The rounding precision of FPCR (MC68881UM 2.2.2)
    fn fpu_rounding_precision(&self) -> Range {
        match self.regs.fpu.fpcr.prec() {
            0b01 => Range::SINGLE,
            0b10 => Range::DOUBLE,
            // 0b11 is undefined in the manual, it must not stop the emulator
            _ => Range::EXTENDED,
        }
    }

    /// e^x - 1 keeps the precision of small x, with more bits still for the subtraction
    fn fpu_etoxm1(x: &Float) -> Float {
        let high = x.get_semantics().increase_precision(128);
        let one = Float::one(high, false);
        (Float::e(high).pow(&x.cast(high)) - one).cast(x.get_semantics())
    }

    /// ln(1 + x) keeps the precision of small x, the same way
    fn fpu_lognp1(x: &Float) -> Float {
        let high = x.get_semantics().increase_precision(128);
        (x.cast(high) + Float::one(high, false))
            .log()
            .cast(x.get_semantics())
    }

    /// FMOVE of FPn to an integer between `min` and `max` of `size` bits: rounds with the
    /// rounding mode of FPCR. Inexact sets INEX2. Not fitting sets OPERR and gives the
    /// largest integer of that sign, and a NAN gives the top bits of its mantissa, with
    /// the SNAN bit set (MC68881UM 6.1.2, 6.1.3).
    pub(in crate::cpu_m68k) fn fpu_to_integer(
        &mut self,
        fpx: usize,
        min: i64,
        max: i64,
        size: u32,
    ) -> i64 {
        let value = self.regs.fpu.fp[fpx];
        let limit = if value.s() { min } else { max };
        match arith::kind(&value) {
            arith::Kind::Zero => 0,
            arith::Kind::Num(a) => {
                let (whole, inexact) = arith::round_to_integer(a, self.fpu_rounding_mode());
                let magnitude = whole.map_or(0, |w| {
                    if w.exp < 63 {
                        (w.man >> (127 - w.exp as u32)) as i64
                    } else {
                        i64::MAX
                    }
                });
                let signed = if a.neg { -magnitude } else { magnitude };
                if (min..=max).contains(&signed) {
                    self.regs.fpu.fpsr.exs_mut().set_inex2(inexact);
                    signed
                } else {
                    self.regs.fpu.fpsr.exs_mut().set_operr(true);
                    limit
                }
            }
            arith::Kind::Inf => {
                self.regs.fpu.fpsr.exs_mut().set_operr(true);
                limit
            }
            arith::Kind::Nan => {
                // MC68881UM 6.1.2 for a signaling one, 6.1.3 for the others
                let snan = value.is_snan();
                self.regs.fpu.fpsr.exs_mut().set_snan(snan);
                self.regs.fpu.fpsr.exs_mut().set_operr(!snan);
                let bits = value.raw_mantissa() | (1 << 62);
                (bits >> (64 - size)) as i64
            }
        }
    }

    /// FMOVE of FPn to the binary real format of `to`: rounded to it, with what that sets
    /// in FPSR (MC68881UM FMOVE). A signaling NAN sets SNAN and has its SNAN bit set
    /// (MC68881UM 6.1.2). Nothing is an operand error here (MC68881UM 6.1.3).
    pub(in crate::cpu_m68k) fn fpu_to_real(&mut self, fpx: usize, to: Range) -> BitsExtReal {
        let value = self.regs.fpu.fp[fpx];
        match arith::kind(&value) {
            arith::Kind::Nan => {
                self.regs.fpu.fpsr.exs_mut().set_snan(value.is_snan());
                value.with_f(value.f() | (1 << 62))
            }
            arith::Kind::Num(a) => self.fpu_finish(Value::Round(a), to),
            arith::Kind::Inf => BitsExtReal::inf(value.s()),
            arith::Kind::Zero => BitsExtReal::zero(value.s()),
        }
    }

    /// The rounded constant at `offset` of FMOVECR
    pub(in crate::cpu_m68k) fn fpu_rom_constant(&mut self, offset: u8) -> BitsExtReal {
        // The mantissa only, as the M68040 FPSP does: the exponent of 10^4096 stays
        let to = Range {
            bits: self.fpu_rounding_precision().bits,
            ..Range::EXTENDED
        };
        let Some((value, inexact)) = constants::constant(offset, self.fpu_rounding_mode()) else {
            return BitsExtReal::zero(false);
        };
        self.regs.fpu.fpsr.exs_mut().set_inex2(inexact);
        self.fpu_finish(Value::Round(value), to)
    }

    /// The source of a function that can't handle the special values: the table of
    /// each instruction in section 4 of the MC68881UM gives their results
    fn fpu_special_function(opmode: u8, source: &BitsExtReal) -> Option<Value> {
        let neg = source.s();
        let same_zero = Value::Exact(BitsExtReal::zero(neg));
        let plus_inf = Value::Exact(BitsExtReal::inf(false));
        let one = |neg| Value::Exact(BitsExtReal::from(&Float::one(SEMANTICS_EXTENDED, neg)));
        let inf = source.is_inf();
        let zero = source.is_zero();
        // Whether the magnitude is above 1 or is 1
        let (above_one, is_one) = match arith::kind(source) {
            arith::Kind::Num(a) => (
                a.exp > 0 || (a.exp == 0 && a.man != 1 << 127),
                a.exp == 0 && a.man == 1 << 127,
            ),
            _ => (false, false),
        };
        match opmode {
            opmode::FSIN | opmode::FTAN if inf => Some(Value::OpErr),
            opmode::FCOS if inf => Some(Value::OpErr),
            opmode::FSIN | opmode::FTAN | opmode::FSINH | opmode::FTANH if zero => Some(same_zero),
            opmode::FASIN | opmode::FATAN | opmode::FATANH | opmode::FETOXM1 if zero => {
                Some(same_zero)
            }
            opmode::FLOGNP1 if zero => Some(same_zero),
            opmode::FCOS | opmode::FCOSH if zero => Some(one(false)),
            opmode::FETOX | opmode::FTWOTOX | opmode::FTENTOX if zero => Some(one(false)),
            opmode::FACOS | opmode::FASIN if inf || above_one => Some(Value::OpErr),
            opmode::FACOS if is_one && !neg => Some(Value::Exact(BitsExtReal::zero(false))),
            opmode::FATANH if inf || above_one => Some(Value::OpErr),
            // This differs from the manuals, which say that +1 gives minus infinity and
            // -1 plus infinity (MC68881UM 6.1.6, M68000PRM FATANH): that's the wrong sign
            // for this function, and the M68040 FPSP gives infinity with the sign of the
            // source, as seen running the ROM's on the Quadra 700. Not checked on a real
            // 6888x: FPUCheck has this for when there's one.
            opmode::FATANH if is_one => Some(Value::DivZero(neg)),
            opmode::FSINH if inf => Some(Value::Exact(*source)),
            opmode::FCOSH if inf => Some(plus_inf),
            opmode::FTANH if inf => Some(one(neg)),
            opmode::FETOX | opmode::FTWOTOX | opmode::FTENTOX if inf => Some(if neg {
                Value::Exact(BitsExtReal::zero(false))
            } else {
                plus_inf
            }),
            opmode::FETOXM1 if inf => Some(if neg { one(true) } else { plus_inf }),
            opmode::FLOGN | opmode::FLOG2 | opmode::FLOG10 => {
                if zero {
                    Some(Value::DivZero(true))
                } else if neg {
                    Some(Value::OpErr)
                } else if inf {
                    Some(plus_inf)
                } else if is_one {
                    Some(Value::Exact(BitsExtReal::zero(false)))
                } else {
                    None
                }
            }
            opmode::FLOGNP1 => {
                if neg && is_one {
                    Some(Value::DivZero(true))
                } else if neg && (inf || above_one) {
                    Some(Value::OpErr)
                } else if inf {
                    Some(plus_inf)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// A transcendental function of an in-range source
    fn fpu_function(opmode: u8, source: &BitsExtReal) -> Value {
        if let Some(special) = Self::fpu_special_function(opmode, source) {
            return special;
        }
        // These run in the default mode, as only their final result gets the rounding
        // mode (MC68881UM 4.5.5.2), with many more bits than the result has
        let near = &Float::from(*source).cast(SEMANTICS_FUNCTIONS);
        let sem = near.get_semantics();

        // So small that the first term of the series is all that counts, for the ones
        // that start at x, and that the series fails to say on subtractions. They are
        // inexact as well, as the rest of the series is there.
        if near.get_exp() < -(sem.get_precision() as i64) {
            let tiny = arith::from_float(near, near.is_negative());
            let one = arith::from_float(&Float::one(sem, false), false);
            match opmode {
                opmode::FSIN
                | opmode::FTAN
                | opmode::FASIN
                | opmode::FATAN
                | opmode::FSINH
                | opmode::FTANH
                | opmode::FATANH
                | opmode::FETOXM1
                | opmode::FLOGNP1 => return tiny,
                opmode::FCOS
                | opmode::FCOSH
                | opmode::FETOX
                | opmode::FTWOTOX
                | opmode::FTENTOX => {
                    return one;
                }
                _ => (),
            }
        }

        let result = match opmode {
            opmode::FACOS => near.acos(),
            opmode::FCOS => near.cos(),
            opmode::FATAN => near.atan(),
            opmode::FSIN => near.sin(),
            opmode::FASIN => near.asin(),
            opmode::FTAN => near.tan(),
            opmode::FLOGN => near.log(),
            opmode::FLOGNP1 => Self::fpu_lognp1(near),
            opmode::FLOG2 => near.log2(),
            opmode::FLOG10 => near.log10(),
            opmode::FETOX => Float::e(sem).pow(near),
            opmode::FETOXM1 => Self::fpu_etoxm1(near),
            opmode::FTWOTOX => Float::from_u64(sem, 2).pow(near),
            opmode::FTENTOX => Float::from_u64(sem, 10).pow(near),
            opmode::FSINH => near.sinh(),
            opmode::FCOSH => near.cosh(),
            opmode::FTANH => near.tanh(),
            opmode::FATANH => near.atanh(),
            _ => unreachable!("not a function"),
        };
        arith::from_float(&result, near.is_negative())
    }

    /// Applies the end of an arithmetic instruction to what it computed: a rounded
    /// result and the exception status (MC68881UM 4.5.5.2)
    fn fpu_finish(&mut self, value: Value, to: Range) -> BitsExtReal {
        match value {
            Value::Exact(bits) => bits,
            Value::OpErr => {
                self.regs.fpu.fpsr.exs_mut().set_operr(true);
                BitsExtReal::nan(false)
            }
            Value::DivZero(neg) => {
                self.regs.fpu.fpsr.exs_mut().set_dz(true);
                BitsExtReal::inf(neg)
            }
            Value::Round(unpacked) => {
                self.regs.fpu.unrounded = Some(unpacked);
                let rounded = arith::round(unpacked, self.fpu_rounding_mode(), to);
                // Added to what's there already: FSINCOS has two results, and whole
                // numbers have inexact bits of their own
                let fpsr = &mut self.regs.fpu.fpsr;
                let (inex2, ovfl, unfl) =
                    (fpsr.exs().inex2(), fpsr.exs().ovfl(), fpsr.exs().unfl());
                fpsr.exs_mut().set_inex2(inex2 || rounded.inexact);
                fpsr.exs_mut().set_ovfl(ovfl || rounded.overflow);
                fpsr.exs_mut().set_unfl(unfl || rounded.underflow);
                if rounded.overflow || rounded.underflow {
                    self.regs.fpu.exc_operand = arith::wrapped_operand(
                        unpacked,
                        self.fpu_rounding_mode(),
                        rounded.overflow,
                    );
                }
                rounded.bits
            }
        }
    }

    pub(in crate::cpu_m68k) fn fpu_alu_op(
        &mut self,
        opmode: u8,
        source: &BitsExtReal,
        dest: &BitsExtReal,
    ) -> Result<BitsExtReal> {
        // The result gets the rounding precision of FPCR (MC68881UM 3.5.2), or
        // that of the instruction on the 68040
        let (opmode, to) = match opmode::rounded(opmode) {
            Some((op, double)) if FPU_TYPE == FPU_M68040 => {
                (op, if double { Range::DOUBLE } else { Range::SINGLE })
            }
            _ => (opmode, self.fpu_rounding_precision()),
        };
        let rm = self.fpu_rounding_mode();

        // Each arithmetic instruction starts its exception status anew, and EXC PEND with
        // it (MC68881UM 6.4.2.2)
        self.regs.fpu.fpsr.exs_mut().0 = 0;
        self.regs.fpu.exc_pending = false;
        self.regs.fpu.unrounded = None;

        let mut cos_register = None;
        let dyadic = opmode::is_dyadic(opmode);
        let result = if source.is_nan() || (dyadic && dest.is_nan()) {
            // Of two NANs the destination's is the result, and a signaling one
            // becomes non-signaling (MC68881UM 4.5.4)
            let snan = source.is_snan() || (dyadic && dest.is_snan());
            self.regs.fpu.fpsr.exs_mut().set_snan(snan);
            let nan = if dyadic && dest.is_nan() {
                dest
            } else {
                source
            };
            let quiet = nan.with_f(nan.f() | (1 << 62));
            if matches!(opmode, opmode::FCMP | opmode::FTST) {
                self.fpu_condition_codes(&quiet);
                let excs = self.regs.fpu.fpsr;
                self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
                return Ok(self.fpu_trap_result(*dest, source, dest, None));
            }
            if matches!(opmode, opmode::FSINCOS..=opmode::FSINCOS_LAST) {
                let register = usize::from(opmode & 0b111);
                cos_register = Some((register, self.regs.fpu.fp[register]));
                self.regs.fpu.fp[register] = quiet;
            }
            if matches!(opmode, opmode::FMOD | opmode::FREM) {
                self.regs.fpu.fpsr.set_quotient(0);
                self.regs.fpu.fpsr.set_quotient_s(false);
            }
            quiet
        } else {
            let value = match opmode {
                opmode::FMOVE => match arith::kind(source) {
                    arith::Kind::Num(a) => Value::Round(a),
                    arith::Kind::Zero => Value::Exact(BitsExtReal::zero(source.s())),
                    _ => Value::Exact(BitsExtReal::inf(source.s())),
                },
                opmode::FABS | opmode::FNEG => {
                    let neg = opmode == opmode::FNEG && !source.s();
                    match arith::kind(source) {
                        arith::Kind::Num(a) => Value::Round(Unpacked { neg, ..a }),
                        arith::Kind::Zero => Value::Exact(BitsExtReal::zero(neg)),
                        _ => Value::Exact(BitsExtReal::inf(neg)),
                    }
                }
                opmode::FSQRT => arith::sqrt(source),
                opmode::FADD => arith::add(dest, source, false, rm),
                opmode::FSUB => arith::add(dest, source, true, rm),
                opmode::FMUL => arith::mul(dest, source),
                opmode::FDIV => arith::div(dest, source),
                opmode::FSGLMUL | opmode::FSGLDIV => {
                    // The mantissa of each operand is truncated to single precision
                    // first, and the rounding precision of FPCR is ignored (MC68881UM
                    // 4.5.5.2). From its first bit: operands are normalized in the FPU, as
                    // the M68040FPSP has it for a denormalized one, which isn't zero then.
                    let truncated = |bits: &BitsExtReal| {
                        let man = bits.raw_mantissa();
                        let kept = 0xFFFF_FF00_0000_0000u64.checked_shr(man.leading_zeros());
                        bits.with_raw_mantissa(man & kept.unwrap_or(0))
                    };
                    let (d, s) = (truncated(dest), truncated(source));
                    let value = if opmode == opmode::FSGLMUL {
                        arith::mul(&d, &s)
                    } else {
                        arith::div(&d, &s)
                    };
                    let bits = self.fpu_finish(value, Range::SINGLE_MANTISSA);
                    Value::Exact(bits)
                }
                opmode::FINT | opmode::FINTRZ => {
                    let mode = if opmode == opmode::FINTRZ {
                        RoundingMode::Zero
                    } else {
                        rm
                    };
                    match arith::kind(source) {
                        arith::Kind::Num(a) => {
                            let (whole, inexact) = arith::round_to_integer(a, mode);
                            self.regs.fpu.fpsr.exs_mut().set_inex2(inexact);
                            // An extended precision number, whatever the rounding
                            // precision (MC68881UM FINT, FINTRZ)
                            let bits = whole.map_or_else(
                                || BitsExtReal::zero(a.neg),
                                |w| arith::round(w, RoundingMode::Zero, Range::EXTENDED).bits,
                            );
                            Value::Exact(bits)
                        }
                        arith::Kind::Zero => Value::Exact(BitsExtReal::zero(source.s())),
                        _ => Value::Exact(BitsExtReal::inf(source.s())),
                    }
                }
                opmode::FCMP => {
                    let (less, equal) = arith::compare(dest, source);
                    // The infinity bit is always cleared. Equal zeros and infinities
                    // have N from the destination (MC68881UM FCMP)
                    let fpsr = &mut self.regs.fpu.fpsr;
                    fpsr.set_fpcc_nan(false);
                    fpsr.set_fpcc_i(false);
                    fpsr.set_fpcc_z(equal);
                    fpsr.set_fpcc_n(less || (equal && dest.s()));
                    return Ok(*dest);
                }
                opmode::FTST => {
                    self.fpu_condition_codes(source);
                    return Ok(*dest);
                }
                opmode::FREM | opmode::FMOD => match (arith::kind(dest), arith::kind(source)) {
                    (arith::Kind::Inf, _) | (_, arith::Kind::Zero) => {
                        // No quotient: the sign is clear too, as the M68040 FPSP leaves it
                        self.regs.fpu.fpsr.set_quotient(0);
                        self.regs.fpu.fpsr.set_quotient_s(false);
                        Value::OpErr
                    }
                    (arith::Kind::Zero, _) => {
                        self.regs.fpu.fpsr.set_quotient(0);
                        self.regs.fpu.fpsr.set_quotient_s(dest.s() != source.s());
                        Value::Exact(BitsExtReal::zero(dest.s()))
                    }
                    (arith::Kind::Num(a), arith::Kind::Inf) => {
                        self.regs.fpu.fpsr.set_quotient(0);
                        self.regs.fpu.fpsr.set_quotient_s(dest.s() != source.s());
                        Value::Round(a)
                    }
                    (arith::Kind::Num(a), arith::Kind::Num(b)) => {
                        let (rest, neg_quotient, quotient) =
                            arith::remainder(a, b, opmode == opmode::FREM);
                        self.regs.fpu.fpsr.set_quotient(quotient);
                        self.regs.fpu.fpsr.set_quotient_s(neg_quotient);
                        rest.map_or_else(|| Value::Exact(BitsExtReal::zero(a.neg)), Value::Round)
                    }
                    _ => unreachable!("NANs are handled before"),
                },
                opmode::FGETEXP => match arith::kind(source) {
                    arith::Kind::Inf => Value::OpErr,
                    arith::Kind::Zero => Value::Exact(*source),
                    arith::Kind::Num(a) if a.exp == 0 => Value::Exact(BitsExtReal::zero(false)),
                    arith::Kind::Num(a) => {
                        let magnitude = u128::from(a.exp.unsigned_abs());
                        let top = 127 - magnitude.leading_zeros();
                        Value::Round(Unpacked {
                            neg: a.exp < 0,
                            exp: top as i32,
                            man: magnitude << (127 - top),
                            sticky: false,
                        })
                    }
                    arith::Kind::Nan => unreachable!("NANs are handled before"),
                },
                opmode::FGETMAN => match arith::kind(source) {
                    arith::Kind::Inf => Value::OpErr,
                    arith::Kind::Zero => Value::Exact(*source),
                    // An extended precision number, whatever the rounding precision
                    // (MC68881UM FGETMAN)
                    arith::Kind::Num(a) => Value::Exact(
                        arith::round(Unpacked { exp: 0, ..a }, rm, Range::EXTENDED).bits,
                    ),
                    arith::Kind::Nan => unreachable!("NANs are handled before"),
                },
                opmode::FSCALE => match (arith::kind(dest), arith::kind(source)) {
                    (_, arith::Kind::Inf) => Value::OpErr,
                    (arith::Kind::Zero | arith::Kind::Inf, _) => Value::Exact(*dest),
                    (arith::Kind::Num(a), arith::Kind::Zero) => Value::Round(a),
                    (arith::Kind::Num(a), arith::Kind::Num(by)) => {
                        Value::Round(arith::scale(a, by))
                    }
                    _ => unreachable!("NANs are handled before"),
                },
                opmode::FSINCOS..=opmode::FSINCOS_LAST => {
                    let register = usize::from(opmode & 0b111);
                    cos_register = Some((register, self.regs.fpu.fp[register]));
                    // M6888x UM: "If FPc and FPs specify the same floating-point data
                    // register, the sine result is stored in the register, and the
                    // cosine result is discarded."
                    //
                    // So we store the cosine result first, which will be overwritten
                    // by the sine result later if destination registers are the same.
                    let cos = Self::fpu_function(opmode::FCOS, source);
                    self.regs.fpu.fp[usize::from(opmode & 0b111)] = self.fpu_finish(cos, to);
                    Self::fpu_function(opmode::FSIN, source)
                }
                opmode::FACOS
                | opmode::FCOS
                | opmode::FATAN
                | opmode::FSIN
                | opmode::FASIN
                | opmode::FTAN
                | opmode::FLOGN
                | opmode::FLOGNP1
                | opmode::FLOG2
                | opmode::FLOG10
                | opmode::FETOX
                | opmode::FETOXM1
                | opmode::FTWOTOX
                | opmode::FTENTOX
                | opmode::FSINH
                | opmode::FCOSH
                | opmode::FTANH
                | opmode::FATANH => Self::fpu_function(opmode, source),

                _ => bail!(CpuError::IllegalInstruction),
            };
            self.fpu_finish(value, to)
        };

        // Condition codes (3.6.2)
        self.fpu_condition_codes(&result);

        let excs = self.regs.fpu.fpsr;
        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());

        Ok(self.fpu_trap_result(result, source, dest, cos_register))
    }

    /// The type of exception that's enabled and has happened, the highest priority one
    /// (MC68881UM 6.1.9), from FPSR and FPCR (MC68881UM 6.4.2.2)
    pub(in crate::cpu_m68k) fn fpu_trap_vector(&self) -> Option<Address> {
        self.fpu_vector_with(0)
    }

    /// The same, with the exceptions in `always` taken whether they are enabled or not
    pub(in crate::cpu_m68k) fn fpu_vector_with(&self, always: Long) -> Option<Address> {
        let fpsr = self.regs.fpu.fpsr.0;
        let active = (fpsr >> 8) & ((self.regs.fpu.fpcr.0 >> 8) | always) & 0xFF;
        [
            (1 << 7, VECTOR_FP_BSUN),
            (1 << 6, VECTOR_FP_SNAN),
            (1 << 5, VECTOR_FP_OPERR),
            (1 << 4, VECTOR_FP_OVFL),
            (1 << 3, VECTOR_FP_UNFL),
            (1 << 2, VECTOR_FP_DZ),
            (3, VECTOR_FP_INEX),
        ]
        .into_iter()
        .find_map(|(bits, vector)| (active & bits != 0).then_some(vector))
    }

    /// What's written to the destination given the enabled exceptions, which make an
    /// exception pending. With SNAN, OPERR and DZ the destination is not modified, with
    /// the source as the exceptional operand; OVFL, UNFL and INEX store as if disabled
    /// (MC68881UM 6.1.2 to 6.1.8).
    fn fpu_trap_result(
        &mut self,
        result: BitsExtReal,
        source: &BitsExtReal,
        dest: &BitsExtReal,
        cos_register: Option<(usize, BitsExtReal)>,
    ) -> BitsExtReal {
        let vector = if FPU_TYPE == FPU_M68040 {
            self.fpu_vector_with(NONMASKABLE_040)
        } else {
            self.fpu_trap_vector()
        };
        self.regs.fpu.exc_pending = vector.is_some();
        match vector {
            Some(VECTOR_FP_SNAN | VECTOR_FP_OPERR | VECTOR_FP_DZ) => {
                self.regs.fpu.exc_operand = *source;
                if let Some((register, old)) = cos_register {
                    self.regs.fpu.fp[register] = old;
                }
                *dest
            }
            // The register isn't affected, the handler writes it (MC68040UM 9.7.4.2, 9.7.5.2)
            Some(VECTOR_FP_OVFL | VECTOR_FP_UNFL) if FPU_TYPE == FPU_M68040 => *dest,
            _ => result,
        }
    }

    /// An enabled exception of an arithmetic instruction on the MC68040, reported by the
    /// next floating-point instruction but FSAVE: E3 for what comes from the result, E1 for
    /// the others (MC68040UM 9.7, 9.8).
    pub(in crate::cpu_m68k) fn fpu_exception040(
        &mut self,
        extword: FmoveExtWord,
        source: ([Long; 3], u8),
        dest: ([Long; 3], u8),
    ) {
        let Some(vector) = self.fpu_vector_with(NONMASKABLE_040) else {
            return;
        };
        let on_result = matches!(vector, VECTOR_FP_OVFL | VECTOR_FP_UNFL | VECTOR_FP_INEX);
        let fpiar = self.regs.fpu.fpiar;
        let frame = match self.regs.fpu.unrounded {
            Some(unrounded) if on_result && opmode::writeback_stage(extword.opmode()) => {
                Busy040::new(extword.0, source, dest, fpiar, false).with_writeback(unrounded)
            }
            // The handlers look for the operand in ETEMP for OVFL, in FPTEMP for UNFL
            // (the M68040FPSP, x_ovfl and x_unfl). They take the sign from it and not from the
            // instruction, so it's there after FABS or FNEG: the manual doesn't say, found
            // with these results through the FPSP against the MC68882 path.
            _ if on_result => {
                let (mut operand, tag) = source;
                let opmode = extword.opmode();
                match opmode::rounded(opmode).map_or(opmode, |(op, _)| op) {
                    opmode::FABS => operand[0] &= 0x7FFF_FFFF,
                    opmode::FNEG => operand[0] ^= 0x8000_0000,
                    _ => {}
                }
                let operand = (operand, tag);
                Busy040::new(extword.0, operand, operand, fpiar, false)
            }
            _ => Busy040::new(extword.0, source, dest, fpiar, false),
        };
        self.regs.fpu.busy040 = Some(frame);
        self.regs.fpu.pending040 = Some(vector);
    }

    /// Reports a pending exception at the start of a floating-point instruction, which
    /// runs again after the handler: a pre-instruction exception (MC68881UM 6.1, 6.1.13).
    /// The acknowledge clears it on the MC68881, and on the MC68882 it stays until the
    /// handler's FRESTORE clears it (MC68881UM 6.1.13, 6.4.2.2). Returns true if taken.
    pub(in crate::cpu_m68k) fn fpu_take_pending(&mut self) -> Result<bool> {
        if FPU_TYPE == FPU_M68040 {
            // The handler's FSAVE takes it out
            let Some(vector) = self.regs.fpu.pending040 else {
                return Ok(false);
            };
            if let Some(regs) = self.restart_regs.take() {
                regs.restore(&mut self.regs);
            }
            // The manual doesn't say which frame a restored T takes. A format $3 one, with the
            // address of the FMOVE, is the only one that works: gen_except sends an FMOVE to
            // memory here, and store in the M68040FPSP writes the result through that address.
            // A format $0 frame has none, so the FPSP wrote through what's above it on the
            // stack: found with FMOVE.X of a denormalized number and UNFL enabled.
            if self
                .regs
                .fpu
                .busy040
                .is_some_and(|busy| busy.post_instruction())
            {
                self.step_fpu_post = Some(self.regs.fpu.move_ea040);
            }
            self.raise_exception(ExceptionGroup::Group2, vector, None)?;
            return Ok(true);
        }
        if !self.regs.fpu.exc_pending {
            return Ok(false);
        }
        let Some(vector) = self.fpu_trap_vector() else {
            return Ok(false);
        };
        if FPU_TYPE == FPU_M68881 {
            self.regs.fpu.exc_pending = false;
        }
        if let Some(regs) = self.restart_regs.take() {
            regs.restore(&mut self.regs);
        }
        self.raise_exception(ExceptionGroup::Group2, vector, None)?;
        Ok(true)
    }

    /// The BSUN of a conditional instruction: reported before the predicate is evaluated,
    /// and the instruction runs again after the handler (MC68881UM 6.1.1). Returns true if
    /// taken.
    pub(in crate::cpu_m68k) fn fpu_bsun_trap(&mut self, bsun: bool) -> Result<bool> {
        if !bsun || !self.regs.fpu.fpcr.exc().bsun() {
            return Ok(false);
        }
        if let Some(regs) = self.restart_regs.take() {
            regs.restore(&mut self.regs);
        }
        self.raise_exception(ExceptionGroup::Group2, VECTOR_FP_BSUN, None)?;
        Ok(true)
    }

    /// FMOVE to memory or a data register reports an exception right after the store, in
    /// the middle of the instruction, with the effective address (MC68881UM 6.1). The
    /// operand on overflow, underflow or a signaling NAN is what was to be stored
    /// (MC68881UM 6.1.2, 6.1.4, 6.1.5).
    ///
    /// On the MC68040 it's a post-instruction exception, with the source in the frame for
    /// the handler (MC68040UM 9.7, table 9-16).
    pub(in crate::cpu_m68k) fn fpu_store_trap(
        &mut self,
        ea: Option<Address>,
        extword: FmoveExtWord,
    ) -> Result<()> {
        // To an integer, a signaling NAN is taken at once too (MC68040UM 9.7.2.2)
        let always = if FPU_TYPE != FPU_M68040 {
            0
        } else if extword.dest_is_integer() {
            NONMASKABLE_040 | EXC_SNAN
        } else {
            NONMASKABLE_040
        };
        let Some(vector) = self.fpu_vector_with(always) else {
            return Ok(());
        };
        if FPU_TYPE == FPU_M68040 {
            let source = Unimp040::operand(&self.regs.fpu.fp[extword.src_reg()]);
            let fpiar = self.regs.fpu.fpiar;
            self.regs.fpu.busy040 = Some(Busy040::new(extword.0, source, source, fpiar, true));
            self.prefetch_pump()?;
            self.regs.fpu.move_ea040 = ea.unwrap_or(0);
            self.step_fpu_post = Some(ea.unwrap_or(0));
            return self.raise_exception(ExceptionGroup::Group2, vector, None);
        }
        self.regs.fpu.exc_pending = FPU_TYPE == FPU_M68882;
        self.prefetch_pump()?;
        self.step_fpu_mid = Some(ea.unwrap_or(0));
        self.raise_exception(ExceptionGroup::Group2, vector, None)
    }

    /// FMOVE to an integer on the MC68040 of what doesn't fit, or of the largest negative
    /// integer, which the FPU flags by mistake: taken at once, enabled or not, for the handler
    /// to write the destination and tell them apart (MC68040UM 9.7.3.2).
    ///
    /// That leaves OPERR out of FPSR, as the M68040FPSP adds it for a real one only (store_max
    /// in x_operr), and FPTEMP has what it looks for there: the integer in two's complement
    /// with an exponent of $C000 for the mistake, an exponent from $4000 for one that doesn't
    /// fit, per the note at the top of x_operr.
    pub(in crate::cpu_m68k) fn fpu_operr_trap040(
        &mut self,
        ea: Option<Address>,
        extword: FmoveExtWord,
        (whole, bits): (i64, i32),
        real: bool,
    ) -> Result<()> {
        let value = self.regs.fpu.fp[extword.src_reg()];
        let fptemp = if real {
            let exp = match arith::kind(&value) {
                arith::Kind::Num(a) => 0x4000 + (a.exp - (bits - 1)).clamp(0, 0x3FFF) as Long,
                _ => 0x7FFF,
            };
            [(Long::from(value.s()) << 31) | (exp << 16), 0, 0]
        } else {
            [0xC000_0000, 0xFFFF_FFFF, whole as Long]
        };
        let source = Unimp040::operand(&value);
        let fpiar = self.regs.fpu.fpiar;
        self.regs.fpu.busy040 = Some(Busy040::new(extword.0, source, (fptemp, 0), fpiar, true));
        self.prefetch_pump()?;
        self.regs.fpu.move_ea040 = ea.unwrap_or(0);
        self.step_fpu_post = Some(ea.unwrap_or(0));
        self.raise_exception(ExceptionGroup::Group2, VECTOR_FP_OPERR, None)
    }

    /// The BSUN bit of a conditional instruction, which also goes in AIOP (MC68881UM 2.3.4)
    pub(in crate::cpu_m68k) fn fpu_set_bsun(&mut self, bsun: bool) {
        self.regs.fpu.fpsr.exs_mut().set_bsun(bsun);
        let excs = self.regs.fpu.fpsr;
        self.regs.fpu.fpsr.aexc_mut().accrue(&excs.exs());
    }

    /// The condition codes of a result (MC68881UM 4.5.5.1)
    pub(in crate::cpu_m68k) fn fpu_condition_codes(&mut self, result: &BitsExtReal) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.s());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
