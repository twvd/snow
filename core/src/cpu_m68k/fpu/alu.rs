use crate::cpu_m68k::cache::CpuCaches;
use crate::cpu_m68k::{FPU_M68040, FpuM68kType};
use anyhow::{Result, bail};
use arpfloat::{Float, RoundingMode};

use crate::bus::{Address, Bus, IrqSource};

use crate::cpu_m68k::CpuM68kType;
use crate::cpu_m68k::cpu::{CpuError, CpuM68k};
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::fpu::math::FloatMath;
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::cpu_m68k::fpu::trig::FloatTrig;

use super::SEMANTICS_EXTENDED;
use super::arith::{self, Range, Unpacked, Value};

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

    /// The rounding precision of FPCR (MC68881UM 2.2.2)
    fn fpu_rounding_precision(&self) -> Range {
        match self.regs.fpu.fpcr.prec() {
            0b01 => Range::SINGLE,
            0b10 => Range::DOUBLE,
            // 0b11 is undefined in the manual, it must not stop the emulator
            _ => Range::EXTENDED,
        }
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
        // mode (MC68881UM 4.5.5.2)
        let near = &Float::from(*source);
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
            opmode::FETOX => Float::e(SEMANTICS_EXTENDED).pow(near),
            opmode::FETOXM1 => Self::fpu_etoxm1(near),
            opmode::FTWOTOX => Float::from_u64(SEMANTICS_EXTENDED, 2).pow(near),
            opmode::FTENTOX => Float::from_u64(SEMANTICS_EXTENDED, 10).pow(near),
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
                let rounded = arith::round(unpacked, self.fpu_rounding_mode(), to);
                // Added to what's there already: FSINCOS has two results, and whole
                // numbers have inexact bits of their own
                let fpsr = &mut self.regs.fpu.fpsr;
                let (inex2, ovfl, unfl) =
                    (fpsr.exs().inex2(), fpsr.exs().ovfl(), fpsr.exs().unfl());
                fpsr.exs_mut().set_inex2(inex2 || rounded.inexact);
                fpsr.exs_mut().set_ovfl(ovfl || rounded.overflow);
                fpsr.exs_mut().set_unfl(unfl || rounded.underflow);
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

        // Each arithmetic instruction starts its exception status anew
        self.regs.fpu.fpsr.exs_mut().0 = 0;

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
                return Ok(*dest);
            }
            if matches!(opmode, opmode::FSINCOS..=opmode::FSINCOS_LAST) {
                self.regs.fpu.fp[usize::from(opmode & 0b111)] = quiet;
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

        Ok(result)
    }

    /// The condition codes of a result (MC68881UM 4.5.5.1)
    pub(in crate::cpu_m68k) fn fpu_condition_codes(&mut self, result: &BitsExtReal) {
        self.regs.fpu.fpsr.set_fpcc_nan(result.is_nan());
        self.regs.fpu.fpsr.set_fpcc_i(result.is_inf());
        self.regs.fpu.fpsr.set_fpcc_n(result.s());
        self.regs.fpu.fpsr.set_fpcc_z(result.is_zero());
    }
}
