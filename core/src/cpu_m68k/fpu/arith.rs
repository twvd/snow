//! The arithmetic of the FPU on exact values.
//!
//! An intermediate result with more bits than any format, then the rounding that ends
//! every arithmetic instruction (MC68881UM 4.5.5.2, 6.1.4, 6.1.5, figure 6-3).

use arpfloat::{Float, RoundingMode};

use super::storage::BitsExtReal;

/// The size of the mantissa and the range of the exponent of a result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// Bits of the mantissa
    pub bits: u32,
    /// Largest exponent
    pub emax: i32,
    /// Smallest exponent of a normalized number
    pub emin: i32,
}

impl Range {
    /// Exponent 0 of this format is normalized if the integer bit is set
    /// (MC68881UM table 3-3)
    pub const EXTENDED: Self = Self {
        bits: 64,
        emax: 16383,
        emin: -16383,
    };
    pub const DOUBLE: Self = Self {
        bits: 53,
        emax: 1023,
        emin: -1022,
    };
    pub const SINGLE: Self = Self {
        bits: 24,
        emax: 127,
        emin: -126,
    };
    /// FSGLDIV and FSGLMUL round the mantissa to single precision, and keep the
    /// exponent of extended precision (MC68881UM 6.1.4, 6.1.5)
    pub const SINGLE_MANTISSA: Self = Self {
        bits: 24,
        emax: 16383,
        emin: -16383,
    };
}

/// A non-zero finite value: (-1)^neg x 2^exp x 1.f, the top bit of `man` being the
/// integer bit. `sticky` stands for more non-zero bits below `man`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unpacked {
    pub neg: bool,
    pub exp: i32,
    pub man: u128,
    pub sticky: bool,
}

/// What an operand is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zero,
    Inf,
    Nan,
    Num(Unpacked),
}

/// What an operation gives, before the end of the instruction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// Exactly these bits, with no rounding
    Exact(BitsExtReal),
    /// A value to round
    Round(Unpacked),
    /// An operand error: a NAN (MC68881UM 6.1.3)
    OpErr,
    /// A divide by zero: an infinity with this sign (MC68881UM 6.1.6)
    DivZero(bool),
}

/// A rounded result, and what happened on the way
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rounded {
    pub bits: BitsExtReal,
    pub inexact: bool,
    pub overflow: bool,
    pub underflow: bool,
}

/// The bias of the extended precision exponent
const BIAS: i32 = 16383;

/// What an operand is.
///
/// Unnormalized numbers count as normalized numbers, denormalized numbers or zero
/// (MC68881UM 3.2.2), and all have the same relation to their value, with exponent 0
/// too (MC68881UM table 3-3): (-1)^s x 2^(e - 16383) x j.f
pub fn kind(bits: &BitsExtReal) -> Kind {
    if bits.is_nan() {
        Kind::Nan
    } else if bits.is_inf() {
        Kind::Inf
    } else if bits.is_zero() {
        Kind::Zero
    } else {
        let man = bits.raw_mantissa();
        let shift = man.leading_zeros();
        Kind::Num(Unpacked {
            neg: bits.s(),
            exp: bits.e() as i32 - BIAS - shift as i32,
            man: u128::from(man << shift) << 64,
            sticky: false,
        })
    }
}

/// `value` as a result with more bits than it has, for a result that is not exact.
/// A zero from a function that can't reach it stands for something too small.
pub fn from_float(value: &Float, neg_if_zero: bool) -> Value {
    if value.is_nan() {
        return Value::OpErr;
    }
    if value.is_inf() {
        return Value::Round(Unpacked {
            neg: value.is_negative(),
            exp: 1 << 20,
            man: 1 << 127,
            sticky: true,
        });
    }
    if value.is_zero() {
        return Value::Round(Unpacked {
            neg: neg_if_zero,
            exp: -(1 << 20),
            man: 1 << 127,
            sticky: true,
        });
    }
    let man = value.get_mantissa().as_u128();
    Value::Round(Unpacked {
        neg: value.is_negative(),
        exp: value.get_exp() as i32,
        man: man << man.leading_zeros(),
        sticky: true,
    })
}

/// Normalizes a non-zero `man` whose top bit stands for 2^exp
fn normal(neg: bool, exp: i32, man: u128, sticky: bool) -> Unpacked {
    let shift = man.leading_zeros();
    Unpacked {
        neg,
        exp: exp - shift as i32,
        man: man << shift,
        sticky,
    }
}

/// Whether to add one to what's left of a mantissa, given the sign, the lowest bit
/// left, the guard bit and all the bits below (MC68881UM figure 6-3)
fn round_up(rm: RoundingMode, neg: bool, odd: bool, guard: bool, rest: bool) -> bool {
    match rm {
        RoundingMode::Negative => neg && (guard || rest),
        RoundingMode::Positive => !neg && (guard || rest),
        RoundingMode::Zero => false,
        _ => guard && (rest || odd),
    }
}

/// The end of an arithmetic instruction: checks for underflow, denormalizing if so,
/// rounds, and checks for overflow (MC68881UM 4.5.5.2)
pub fn round(value: Unpacked, rm: RoundingMode, to: Range) -> Rounded {
    // A value with the smallest exponent is representable, any lower is not. This
    // comes before the rounding, and doesn't depend on whether it's exact: the
    // exception bit is for "any time a tiny number is generated" (MC68881UM 6.1.5).
    let tiny = value.exp < to.emin;
    // The weight of the lowest bit kept: the format's own when denormalizing
    let lowest = if tiny { to.emin } else { value.exp } - (to.bits as i32 - 1);
    // Above that is what's kept, zero if denormalizing takes all of it
    let keep = value.exp - lowest + 1;
    let (kept, guard, rest) = if keep > 0 {
        let shift = 128 - keep as u32;
        let guard = value.man & (1 << (shift - 1)) != 0;
        let below = value.man & ((1 << (shift - 1)) - 1);
        (value.man >> shift, guard, below != 0 || value.sticky)
    } else {
        // Nothing is left, and at most the top bit is the guard bit
        (
            0,
            keep == 0,
            keep != 0 || value.man << 1 != 0 || value.sticky,
        )
    };
    let inexact = guard || rest;
    let kept = kept + u128::from(round_up(rm, value.neg, kept & 1 != 0, guard, rest));
    let sign = BitsExtReal::zero(value.neg);

    if kept == 0 {
        return Rounded {
            bits: sign,
            inexact,
            overflow: false,
            underflow: tiny,
        };
    }

    let top = 127 - kept.leading_zeros() as i32;
    let exp = lowest + top;
    if exp > to.emax {
        // MC68881UM 6.1.4, trap disabled results
        let largest = match rm {
            RoundingMode::Zero => true,
            RoundingMode::Negative => !value.neg,
            RoundingMode::Positive => value.neg,
            _ => false,
        };
        return Rounded {
            bits: if largest {
                sign.with_e((to.emax + BIAS) as u64)
                    .with_raw_mantissa(!0 << (64 - to.bits))
            } else {
                BitsExtReal::inf(value.neg)
            },
            inexact,
            overflow: true,
            underflow: false,
        };
    }

    // A denormalized extended precision number keeps the exponent at 0, with the
    // integer bit clear. Single and double precision ones are normalized numbers of
    // extended precision (MC68881UM 6.1.7).
    let biased = exp + BIAS;
    let bits = if biased < 0 {
        sign.with_raw_mantissa((kept << (lowest + BIAS + 63)) as u64)
    } else {
        sign.with_e(biased as u64)
            .with_raw_mantissa(((kept << (127 - top)) >> 64) as u64)
    };
    Rounded {
        bits,
        inexact,
        overflow: false,
        underflow: tiny,
    }
}

/// `d` + `s`, or `d` - `s` (MC68881UM FADD, FSUB)
pub fn add(d: &BitsExtReal, s: &BitsExtReal, subtract: bool, rm: RoundingMode) -> Value {
    let sneg = s.s() != subtract;
    match (kind(d), kind(s)) {
        (Kind::Inf, Kind::Inf) if d.s() != sneg => Value::OpErr,
        (Kind::Inf, _) => Value::Exact(BitsExtReal::inf(d.s())),
        (_, Kind::Inf) => Value::Exact(BitsExtReal::inf(sneg)),
        (Kind::Zero, Kind::Zero) => {
            // Zeros of different signs give +0, or -0 rounding to minus infinity
            let neg = if d.s() == sneg {
                sneg
            } else {
                rm == RoundingMode::Negative
            };
            Value::Exact(BitsExtReal::zero(neg))
        }
        (Kind::Zero, Kind::Num(b)) => Value::Round(Unpacked { neg: sneg, ..b }),
        (Kind::Num(a), Kind::Zero) => Value::Round(a),
        (Kind::Num(a), Kind::Num(b)) => add_numbers(a, Unpacked { neg: sneg, ..b }, rm),
        _ => unreachable!("NANs are handled before"),
    }
}

fn add_numbers(a: Unpacked, b: Unpacked, rm: RoundingMode) -> Value {
    let (big, small) = if (a.exp, a.man) >= (b.exp, b.man) {
        (a, b)
    } else {
        (b, a)
    };
    // One bit of room for a carry. Operands have 64 bits, so bits only go when the
    // difference in exponents is so large that they all count as a sticky bit.
    let exp = big.exp + 1;
    let top = big.man >> 1;
    let diff = (big.exp - small.exp) as u32 + 1;
    let (low, sticky) = if diff < 128 {
        (small.man >> diff, small.man & ((1 << diff) - 1) != 0)
    } else {
        (0, true)
    };
    let man = if big.neg == small.neg {
        top + low
    } else {
        // The lost bits are below `low`, so the true result is a little less
        top - low - u128::from(sticky)
    };
    if man == 0 {
        // Equal numbers of different signs (MC68881UM FADD)
        return Value::Exact(BitsExtReal::zero(rm == RoundingMode::Negative));
    }
    Value::Round(normal(big.neg, exp, man, sticky))
}

/// `d` x `s` (MC68881UM FMUL)
pub fn mul(d: &BitsExtReal, s: &BitsExtReal) -> Value {
    let neg = d.s() != s.s();
    match (kind(d), kind(s)) {
        (Kind::Inf, Kind::Zero) | (Kind::Zero, Kind::Inf) => Value::OpErr,
        (Kind::Inf, _) | (_, Kind::Inf) => Value::Exact(BitsExtReal::inf(neg)),
        (Kind::Zero, _) | (_, Kind::Zero) => Value::Exact(BitsExtReal::zero(neg)),
        (Kind::Num(a), Kind::Num(b)) => {
            let man = (a.man >> 64) * (b.man >> 64);
            Value::Round(normal(neg, a.exp + b.exp + 1, man, false))
        }
        _ => unreachable!("NANs are handled before"),
    }
}

/// `d` / `s` (MC68881UM FDIV)
pub fn div(d: &BitsExtReal, s: &BitsExtReal) -> Value {
    let neg = d.s() != s.s();
    match (kind(d), kind(s)) {
        (Kind::Inf, Kind::Inf) | (Kind::Zero, Kind::Zero) => Value::OpErr,
        (Kind::Inf, _) => Value::Exact(BitsExtReal::inf(neg)),
        (_, Kind::Inf) | (Kind::Zero, _) => Value::Exact(BitsExtReal::zero(neg)),
        (_, Kind::Zero) => Value::DivZero(neg),
        (Kind::Num(a), Kind::Num(b)) => {
            let divisor = b.man >> 64;
            let dividend = a.man;
            let high = dividend / divisor;
            let rest = dividend % divisor;
            let low = (rest << 64) / divisor;
            let sticky = (rest << 64) % divisor != 0;
            // `high` has 64 bits, and 65 if the dividend's mantissa is not the smaller
            let (man, exp, sticky) = if high >> 64 != 0 {
                (
                    (high << 63) | (low >> 1),
                    a.exp - b.exp,
                    sticky || low & 1 != 0,
                )
            } else {
                ((high << 64) | low, a.exp - b.exp - 1, sticky)
            };
            Value::Round(normal(neg, exp, man, sticky))
        }
        _ => unreachable!("NANs are handled before"),
    }
}

/// The square root (MC68881UM FSQRT), a bit at a time, enough for any rounding with
/// the sticky bit
pub fn sqrt(s: &BitsExtReal) -> Value {
    match kind(s) {
        Kind::Zero => Value::Exact(*s),
        _ if s.s() => Value::OpErr,
        Kind::Inf => Value::Exact(*s),
        Kind::Num(a) => {
            // 1.f or 2 x 1.f, to have an even exponent left, with the integer part in
            // the top two bits
            let odd = a.exp & 1 != 0;
            let mut radicand = (a.man >> 64) << (63 + u32::from(odd));
            let mut root: u128 = 0;
            let mut rest: u128 = 0;
            const STEPS: u32 = 66;
            for _ in 0..STEPS {
                rest = (rest << 2) | (radicand >> 126);
                radicand <<= 2;
                let trial = (root << 2) | 1;
                root <<= 1;
                if rest >= trial {
                    rest -= trial;
                    root |= 1;
                }
            }
            let exp = (a.exp - i32::from(odd)) / 2;
            Value::Round(normal(false, exp, root << (128 - STEPS), rest != 0))
        }
        _ => unreachable!("NANs are handled before"),
    }
}

/// The value rounded to a whole number, and whether that changed it (MC68881UM FINT,
/// FINTRZ)
pub fn round_to_integer(a: Unpacked, rm: RoundingMode) -> (Option<Unpacked>, bool) {
    if a.exp >= 127 {
        return (Some(a), a.sticky);
    }
    let (kept, guard, rest) = if a.exp >= 0 {
        let shift = 127 - a.exp as u32;
        let below = a.man & ((1 << (shift - 1)) - 1);
        (
            a.man >> shift,
            a.man & (1 << (shift - 1)) != 0,
            below != 0 || a.sticky,
        )
    } else {
        // Below 1: the top bit is the guard bit of a whole number for 2^-1
        (0, a.exp == -1, a.exp != -1 || a.man << 1 != 0 || a.sticky)
    };
    let kept = kept + u128::from(round_up(rm, a.neg, kept & 1 != 0, guard, rest));
    let whole = (kept != 0).then(|| normal(a.neg, 127, kept, false));
    (whole, guard || rest)
}

/// The remainder of `d` / `s`, in the sense of FREM if `nearest` or FMOD if not, and
/// the sign and low 7 bits of the quotient (MC68881UM FMOD, FREM)
pub fn remainder(a: Unpacked, b: Unpacked, nearest: bool) -> (Option<Unpacked>, bool, u8) {
    let divisor = b.man >> 64;
    let neg_quotient = a.neg != b.neg;
    let mut rest = a.man >> 64;
    let mut quotient: u8 = 0;
    // Both in units of the divisor's lowest bit, after moving a's mantissa up
    let shifts = a.exp - b.exp;
    for i in (0..=shifts).rev() {
        quotient <<= 1;
        if rest >= divisor {
            rest -= divisor;
            quotient |= 1;
        }
        if i > 0 {
            rest <<= 1;
        }
    }
    // The lowest bit of `rest` is worth this, or that of a if it's the smaller
    let unit = a.exp.min(b.exp) - 63;
    let mut neg = a.neg;
    let quotient_odd = quotient & 1 != 0;
    if nearest {
        // How the remainder compares with half the divisor, in units of `unit`
        let (twice, whole) = if shifts >= 0 {
            (rest << 1, divisor)
        } else if shifts == -1 {
            (rest, divisor)
        } else {
            (0, 1)
        };
        if twice > whole || (twice == whole && quotient_odd) {
            let divisor_in_unit = if shifts >= 0 { divisor } else { divisor << 1 };
            rest = divisor_in_unit - rest;
            quotient = quotient.wrapping_add(1);
            neg = !neg;
        }
    }
    let quotient = quotient & 0x7F;
    if rest == 0 {
        return (None, neg_quotient, quotient);
    }
    let top = 127 - rest.leading_zeros() as i32;
    let value = normal(neg, unit + top, rest << (127 - top), false);
    (Some(value), neg_quotient, quotient)
}

/// FSCALE: adds a whole number to the exponent. From 2^14 on it's always an overflow or
/// an underflow (MC68881UM FSCALE), even for a number that would come back in range.
pub fn scale(a: Unpacked, by: Unpacked) -> Unpacked {
    const OUT_OF_RANGE: i32 = 1 << 20;
    let n = if by.exp >= 14 {
        return Unpacked {
            exp: if by.neg { -OUT_OF_RANGE } else { OUT_OF_RANGE },
            ..a
        };
    } else if by.exp < 0 {
        0
    } else {
        (by.man >> (127 - by.exp as u32)) as i32
    };
    Unpacked {
        exp: a.exp + if by.neg { -n } else { n },
        ..a
    }
}

/// Compares two values, not NANs: whether `d` < `s`, and whether they're equal
/// (MC68881UM FCMP)
pub fn compare(d: &BitsExtReal, s: &BitsExtReal) -> (bool, bool) {
    let magnitude = |bits: &BitsExtReal| match kind(bits) {
        Kind::Zero => (0, 0, 0),
        Kind::Inf => (2, 0, 0),
        Kind::Num(a) => (1, a.exp, a.man),
        Kind::Nan => unreachable!("NANs are handled before"),
    };
    let (dm, sm) = (magnitude(d), magnitude(s));
    if dm == (0, 0, 0) && sm == (0, 0, 0) {
        return (false, true);
    }
    if d.s() != s.s() {
        return (d.s(), false);
    }
    if dm == sm {
        return (false, true);
    }
    (d.s() == (dm > sm), false)
}
