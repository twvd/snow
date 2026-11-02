//! The packed decimal real format, exactly (MC68881UM 3.6, figure 3-11, table 3-4).
//!
//! The real FPU only keeps to error bounds on these conversions (MC68881UM 4.3.3), so
//! its low bits can differ from these correctly rounded ones.

use arpfloat::{BigInt, RoundingMode};

use super::arith::{self, Range, Unpacked};
use super::storage::BitsExtReal;
use crate::types::Long;

/// The exponent bits of infinities and NANs: SE, YY and $FFF (MC68881UM table 3-4)
const SPECIAL: Long = 0x7FFF_0000;

/// The digits of the mantissa, or of the exponent. Digits past 9 count the same way as
/// decimal ones, which is useless but repeatable (MC68881UM table 3-4, note 2).
fn digits(value: u64, count: u32) -> u64 {
    (0..count)
        .rev()
        .fold(0, |sum, i| sum * 10 + ((value >> (4 * i)) & 0xF))
}

fn pow10(n: u32) -> BigInt {
    BigInt::from_u64(10).powi(u64::from(n))
}

/// `num` / `den`, positive, with enough bits for any rounding
fn ratio(neg: bool, mut num: BigInt, mut den: BigInt, exp2: i32) -> Unpacked {
    // At least 128 bits in the quotient
    let shift = 130 + den.msb_index() as i32 - num.msb_index() as i32;
    if shift > 0 {
        num.shift_left(shift as usize);
    } else {
        den.shift_left(-shift as usize);
    }
    let rest = num.inplace_div(&den);
    // `num` is the quotient now, with the lowest bit worth 2^(exp2 - shift)
    let bits = num.msb_index() as i32;
    let mut sticky = !rest.is_zero();
    let extra = (bits - 128).max(0) as usize;
    if extra > 0 {
        sticky = sticky || num.trailing_zeros() < extra;
        num.shift_right(extra);
    }
    let man = num.as_u128();
    Unpacked {
        neg,
        exp: exp2 - shift + bits - 1,
        man: man << man.leading_zeros(),
        sticky,
    }
}

/// A packed decimal real to extended precision, whatever the rounding precision, and
/// whether that's inexact (MC68881UM 6.1.8). A NAN moves bit for bit, and stays signaling
/// or not (table 3-4, note 1).
pub fn from_packed(raw: [Long; 3], rm: RoundingMode) -> (BitsExtReal, bool) {
    let neg = raw[0] & (1 << 31) != 0;
    let fraction = (u64::from(raw[1]) << 32) | u64::from(raw[2]);
    if raw[0] & SPECIAL == SPECIAL {
        let bits = if fraction == 0 {
            BitsExtReal::inf(neg)
        } else {
            BitsExtReal::inf(neg).with_raw_mantissa(fraction)
        };
        return (bits, false);
    }
    let integer = u64::from(raw[0] & 0xF);
    if integer == 0 && fraction == 0 {
        return (BitsExtReal::zero(neg), false);
    }
    let mantissa = integer * 10u64.pow(16) + digits(fraction, 16);
    let exponent = digits(u64::from(raw[0] >> 16) & 0xFFF, 3) as i32;
    let exponent = if raw[0] & (1 << 30) != 0 {
        -exponent
    } else {
        exponent
    } - 16;

    let mut num = BigInt::from_u64(mantissa);
    let den = if exponent < 0 {
        pow10(exponent.unsigned_abs())
    } else {
        num.inplace_mul(&pow10(exponent as u32));
        BigInt::one()
    };
    let rounded = arith::round(ratio(neg, num, den, 0), rm, Range::EXTENDED);
    (rounded.bits, rounded.inexact)
}

/// What FMOVE to packed decimal gives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packed {
    pub raw: [Long; 3],
    pub inexact: bool,
    pub operr: bool,
    pub snan: bool,
}

/// `value` as a packed decimal real (MC68881UM FMOVE).
///
/// A positive `k` is the number of significant digits, others the number of digits right
/// of the decimal point; more than 17 is an operand error and takes 17.
pub fn to_packed(value: BitsExtReal, k: i8, rm: RoundingMode) -> Packed {
    let sign = Long::from(value.s()) << 31;
    let special = |raw, snan| Packed {
        raw,
        inexact: false,
        operr: false,
        snan,
    };
    let a = match arith::kind(&value) {
        arith::Kind::Inf => return special([sign | SPECIAL, 0, 0], false),
        arith::Kind::Nan => {
            // Non-signaling, with its bits otherwise unchanged (MC68881UM 6.1.2)
            let man = value.raw_mantissa() | (1 << 62);
            let raw = [sign | SPECIAL, (man >> 32) as Long, man as Long];
            return special(raw, value.is_snan());
        }
        arith::Kind::Zero => return special([sign, 0, 0], false),
        arith::Kind::Num(a) => a,
    };

    let operr_k = k > 17;
    // The value as num / den, and the power of 10 of its first digit
    let mut num = BigInt::from_u128(a.man >> 64);
    let mut den = BigInt::one();
    let shift = a.exp - 63;
    if shift >= 0 {
        num.shift_left(shift as usize);
    } else {
        den.shift_left(-shift as usize);
    }
    let mut ilog = (i64::from(a.exp) * 30103).div_euclid(100_000) as i32;
    let ge_pow10 = |p: i32| {
        if p >= 0 {
            num >= den.clone() * pow10(p as u32)
        } else {
            num.clone() * pow10(p.unsigned_abs()) >= den
        }
    };
    while !ge_pow10(ilog) {
        ilog -= 1;
    }
    while ge_pow10(ilog + 1) {
        ilog += 1;
    }

    // The MC68881UM says nothing of a value too small for the k-factor, or of a denormalized
    // one, and we don't know what a 6888x does with them. We assume it's what bindec does in
    // the M68040FPSP, which we compared with by running FMOVE.P on the 68040 through it. With
    // k <= 0, a value below 10^k takes 10^k for its first digit, and one digit. A denormalized
    // value starts from 10^-4933, whatever it is, so it can have leading zeros.
    let k_digits = i32::from(k);
    let denormal = !value.i() && a.exp < -16382;
    if denormal {
        ilog = -4933;
    }
    if k <= 0 && ilog < k_digits {
        ilog = k_digits;
    }
    let len = if k > 0 {
        k_digits.min(17)
    } else {
        (ilog + 1 - k_digits).clamp(1, 17)
    };
    // The digits as a whole number: value / 10^(ilog - len + 1), rounded
    let digits_at = |ilog: i32| {
        let scale = ilog - len + 1;
        let (mut q, den_scaled) = if scale >= 0 {
            (num.clone(), den.clone() * pow10(scale as u32))
        } else {
            (num.clone() * pow10(scale.unsigned_abs()), den.clone())
        };
        let rest = q.inplace_div(&den_scaled);
        let mut digits = q.as_u64();
        let inexact = !rest.is_zero();
        if inexact {
            // What's missing to the next one, to compare with what's over
            let missing = den_scaled - rest.clone();
            let up = match rm {
                RoundingMode::Zero => false,
                RoundingMode::Negative => a.neg,
                RoundingMode::Positive => !a.neg,
                _ => rest > missing || (rest == missing && digits & 1 != 0),
            };
            if up {
                digits += 1;
            }
        }
        (digits, inexact)
    };
    let (mut digits_value, mut inexact) = digits_at(ilog);
    if digits_value > 10u64.pow(len as u32) {
        ilog += 1;
        (digits_value, inexact) = digits_at(ilog);
    }
    if digits_value == 10u64.pow(len as u32) {
        digits_value /= 10;
        ilog += 1;
    }
    let digits_value = digits_value * 10u64.pow(17 - len as u32);

    // A fourth exponent digit is an operand error (MC68881UM table 6-2)
    // With no digits the exponent is that of bindec too
    let magnitude = match digits_value {
        0 if !denormal => 1,
        0 if k >= 0 => 4933,
        _ => ilog.unsigned_abs(),
    };
    let operr = operr_k || magnitude > 999;
    let decimal = |mut v: u64, count: u32| {
        (0..count).fold(0u64, |out, i| {
            let digit = v % 10;
            v /= 10;
            out | (digit << (4 * i))
        })
    };
    let fraction = decimal(digits_value % 10u64.pow(16), 16);
    let dw0 = sign
        | (Long::from(ilog < 0) << 30)
        | ((decimal(u64::from(magnitude % 1000), 3) as Long) << 16)
        | (((magnitude / 1000) & 0xF) << 12)
        | (digits_value / 10u64.pow(16)) as Long;
    Packed {
        raw: [dw0, (fraction >> 32) as Long, fraction as Long],
        inexact,
        operr,
        snan: false,
    }
}
