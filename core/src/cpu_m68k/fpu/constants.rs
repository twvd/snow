//! The constants of FMOVECR.
//!
//! MC68881UM FMOVECR only names them, and has no bits. These are from the tables of the
//! M68040 FPSP (get_op and smovecr of the M68040 Software Package, Copyright (c) 1993,
//! 1994 Motorola Inc.), which stand in for the ROM of the 6888x. They aren't always the
//! nearest values: e rounds down, to ...4A9A.

use arpfloat::RoundingMode;

use super::arith::Unpacked;

/// Offset, sign and exponent, mantissa for rounding to nearest, to zero or minus infinity,
/// and to plus infinity, and whether it's an inexact constant
#[rustfmt::skip]
const ROM: [(u8, u16, u64, u64, u64, bool); 21] = [
    // pi
    (0x00, 0x4000, 0xC90FDAA2_2168C235, 0xC90FDAA2_2168C234, 0xC90FDAA2_2168C235, true),
    // log10(2)
    (0x0B, 0x3FFD, 0x9A209A84_FBCFF798, 0x9A209A84_FBCFF798, 0x9A209A84_FBCFF799, true),
    // e
    (0x0C, 0x4000, 0xADF85458_A2BB4A9A, 0xADF85458_A2BB4A9A, 0xADF85458_A2BB4A9B, true),
    // log2(e)
    (0x0D, 0x3FFF, 0xB8AA3B29_5C17F0BC, 0xB8AA3B29_5C17F0BB, 0xB8AA3B29_5C17F0BC, true),
    // log10(e)
    (0x0E, 0x3FFD, 0xDE5BD8A9_37287195, 0xDE5BD8A9_37287195, 0xDE5BD8A9_37287195, false),
    // ln(2)
    (0x30, 0x3FFE, 0xB17217F7_D1CF79AC, 0xB17217F7_D1CF79AB, 0xB17217F7_D1CF79AC, true),
    // ln(10)
    (0x31, 0x4000, 0x935D8DDD_AAA8AC17, 0x935D8DDD_AAA8AC16, 0x935D8DDD_AAA8AC17, true),
    // 10^0
    (0x32, 0x3FFF, 0x80000000_00000000, 0x80000000_00000000, 0x80000000_00000000, false),
    // 10^1
    (0x33, 0x4002, 0xA0000000_00000000, 0xA0000000_00000000, 0xA0000000_00000000, false),
    // 10^2
    (0x34, 0x4005, 0xC8000000_00000000, 0xC8000000_00000000, 0xC8000000_00000000, false),
    // 10^4
    (0x35, 0x400C, 0x9C400000_00000000, 0x9C400000_00000000, 0x9C400000_00000000, false),
    // 10^8
    (0x36, 0x4019, 0xBEBC2000_00000000, 0xBEBC2000_00000000, 0xBEBC2000_00000000, false),
    // 10^16
    (0x37, 0x4034, 0x8E1BC9BF_04000000, 0x8E1BC9BF_04000000, 0x8E1BC9BF_04000000, false),
    // 10^32
    (0x38, 0x4069, 0x9DC5ADA8_2B70B59E, 0x9DC5ADA8_2B70B59D, 0x9DC5ADA8_2B70B59E, true),
    // 10^64
    (0x39, 0x40D3, 0xC2781F49_FFCFA6D5, 0xC2781F49_FFCFA6D5, 0xC2781F49_FFCFA6D6, true),
    // 10^128
    (0x3A, 0x41A8, 0x93BA47C9_80E98CE0, 0x93BA47C9_80E98CDF, 0x93BA47C9_80E98CE0, true),
    // 10^256
    (0x3B, 0x4351, 0xAA7EEBFB_9DF9DE8E, 0xAA7EEBFB_9DF9DE8D, 0xAA7EEBFB_9DF9DE8E, true),
    // 10^512
    (0x3C, 0x46A3, 0xE319A0AE_A60E91C7, 0xE319A0AE_A60E91C6, 0xE319A0AE_A60E91C7, true),
    // 10^1024
    (0x3D, 0x4D48, 0xC9767586_81750C17, 0xC9767586_81750C17, 0xC9767586_81750C18, true),
    // 10^2048
    (0x3E, 0x5A92, 0x9E8B3B5D_C53D5DE5, 0x9E8B3B5D_C53D5DE5, 0x9E8B3B5D_C53D5DE6, true),
    // 10^4096
    (0x3F, 0x7525, 0xC4605202_8A20979B, 0xC4605202_8A20979A, 0xC4605202_8A20979B, true),
];

/// The value at `offset` and whether it's inexact, or `None` for 0.0 and the offsets that
/// the manual reserves, different per mask set. The M68040 FPSP returns 0.0 for those.
pub fn constant(offset: u8, rm: RoundingMode) -> Option<(Unpacked, bool)> {
    let &(_, exp, nearest, down, up, inexact) = ROM.iter().find(|entry| entry.0 == offset)?;
    let man = match rm {
        RoundingMode::Zero | RoundingMode::Negative => down,
        RoundingMode::Positive => up,
        _ => nearest,
    };
    let value = Unpacked {
        neg: false,
        exp: i32::from(exp) - 16383,
        man: u128::from(man) << 64,
        sticky: false,
    };
    Some((value, inexact))
}
