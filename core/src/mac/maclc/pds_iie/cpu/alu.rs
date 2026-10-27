/// Result of an ALU operation
pub struct AluResult {
    pub result: u16,
    pub c: bool,
    pub n: bool,
    pub z: bool,
    pub v: bool,
}

/// 16-bit add (decimal mode)
pub fn add16_dec(acc: u16, d: u16, carry: bool) -> AluResult {
    let acc: u32 = acc.into();
    let d: u32 = d.into();
    let mut c = if carry { 1 } else { 0 };
    let mut result: u32 = (acc & 0x000F) + (d & 0x000F) + c;
    if result > 0x0009 {
        result += 0x0006;
    }
    c = if result > 0x000F { 1 } else { 0 };
    result = (acc & 0x00F0) + (d & 0x00F0) + (c << 4) + (result & 0x000F);
    if result > 0x009F {
        result += 0x0060;
    }

    c = if result > 0x00FF { 1 } else { 0 };
    result = (acc & 0x0F00) + (d & 0x0F00) + (c << 8) + (result & 0x00FF);
    if result > 0x09FF {
        result += 0x0600;
    }
    c = if result > 0x0FFF { 1 } else { 0 };

    result = (acc & 0xF000) + (d & 0xF000) + (c << 12) + (result & 0x0FFF);
    let v = !(acc ^ d) & (acc ^ result) & 0x8000 != 0;
    if result > 0x9FFF {
        result += 0x6000;
    }

    AluResult {
        result: result as u16,
        z: result as u16 == 0,
        n: result & 0x8000 != 0,
        v,
        c: result > u16::MAX.into(),
    }
}

/// 16-bit add (binary mode)
pub fn add16(acc: u16, d: u16, carry: bool) -> AluResult {
    let c = if carry { 1 } else { 0 };

    let result: u32 = acc as u32 + d as u32 + c;

    AluResult {
        result: result as u16,
        z: result as u16 == 0,
        n: result & 0x8000 != 0,
        v: !(acc ^ d) & (acc ^ result as u16) & 0x8000 != 0,
        c: result > u16::MAX.into(),
    }
}

/// 8-bit add (binary mode)
pub fn add8(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let c = if carry { 1 } else { 0 };

    let result: u16 = acc + d + c;

    AluResult {
        result: result & 0xFF,
        z: result as u8 == 0,
        n: result & 0x80 != 0,
        v: !(acc ^ d) & (acc ^ result) & 0x80 != 0,
        c: result > u8::MAX.into(),
    }
}

/// 8-bit add (decimal mode)
pub fn add8_dec(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let mut c = if carry { 1 } else { 0 };
    let mut result: u16 = (acc & 0x000F) + (d & 0x000F) + c;
    if result > 0x0009 {
        result += 0x0006;
    }
    c = if result > 0x000F { 1 } else { 0 };
    result = (acc & 0x00F0) + (d & 0x00F0) + (c << 4) + (result & 0x000F);
    let v = !(acc ^ d) & (acc ^ result) & 0x80 != 0;
    if result > 0x009F {
        result += 0x0060;
    }

    AluResult {
        result: result & 0xFF,
        z: result as u8 == 0,
        n: result & 0x80 != 0,
        v,
        c: result > u8::MAX.into(),
    }
}

/// 16-bit subtract (binary mode)
pub fn sub16(acc: u16, d: u16, carry: bool) -> AluResult {
    let c = if carry { 1 } else { 0 };

    let result: i32 = acc as i32 + (!d) as i32 + c;

    AluResult {
        result: result as u16,
        z: result as u16 == 0,
        n: result & 0x8000 != 0,
        v: !(acc ^ !d) & (acc ^ result as u16) & 0x8000 != 0,
        c: result > u16::MAX.into(),
    }
}

/// 8-bit subtract (binary mode)
pub fn sub8(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let c = if carry { 1 } else { 0 };

    let result = acc.wrapping_add(!d & 0xFF).wrapping_add(c);

    AluResult {
        result: result & 0xFF,
        z: result as u8 == 0,
        n: result & 0x80 != 0,
        v: !(acc ^ !d) & (acc ^ result) & 0x80 != 0,
        c: result > u8::MAX.into(),
    }
}

/// 8-bit subtract (decimal mode)
pub fn sub8_dec(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let d = !d as i16;
    let mut c = if carry { 1 } else { 0 };
    let acc = acc as i16;
    let mut result: i16 = (acc & 0x000F) + (d & 0x000F) + c;
    if result <= 0x000F {
        result -= 0x06;
    }
    c = if result > 0x000F { 1 } else { 0 };
    result = (acc & 0x00F0) + (d & 0x00F0) + (c << 4) + (result & 0x000F);
    let v = !(acc ^ d) & (acc ^ result) & 0x80 != 0;
    if result <= 0x00FF {
        result -= 0x60;
    }

    AluResult {
        result: result as u16 & 0xFF,
        z: result as u8 == 0,
        n: result & 0x80 != 0,
        v,
        c: result > u8::MAX.into(),
    }
}

/// 8-bit subtract (decimal mode) for the WDC 65C02.
///
/// Derived empirically from TomHarte's SingleStepTests wdc65c02 data
/// (verified over all distinct decimal-mode SBC cases).  For invalid
/// BCD operands the WDC 65C02 differs from the textbook algorithm: the
/// low nibble loses 6 when it has no carry out, its borrow propagates
/// into the high nibble only when the corrected low digit exceeds 9,
/// and the high nibble correction is applied before that borrow.
/// N and V follow the binary sum; Z follows the decimal result.
pub fn sub8_dec_wdc65c02(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let inv = !d & 0xFF;
    let bin: u16 = acc + inv + if carry { 1 } else { 0 };

    let c = if carry { 1 } else { 0 };
    let lo = (acc & 0x000F) + (inv & 0x000F) + c;
    let c_lo = if lo > 0x000F { 1 } else { 0 };
    let borrow = lo <= 0x000F;
    let low = if borrow {
        lo.wrapping_sub(0x0006) & 0x000F
    } else {
        lo & 0x000F
    };
    // The borrow from the low-nibble correction propagates only when
    // the corrected digit still exceeds 9.
    let prop = borrow && low > 0x0009;
    let mut hi = (acc >> 4) + (inv >> 4) + c_lo;
    if hi <= 0x000F {
        hi = hi.wrapping_sub(0x0006);
    }
    let hi = hi.wrapping_sub(if prop { 1 } else { 0 }) & 0x000F;

    let result = (hi << 4) | low;

    AluResult {
        result,
        z: result == 0,
        n: result & 0x80 != 0,
        v: !(acc ^ inv) & (acc ^ bin) & 0x80 != 0,
        c: bin > u8::MAX.into(),
    }
}

/// 8-bit subtract (decimal mode) with the NMOS 6502 quirks.
///
/// The NMOS 6502's decimal SBC differs from the 65C02's for invalid BCD
/// operands: each nibble is corrected independently based on the
/// (unadjusted) intermediate nibble sums of `A + ~M + C` — the low
/// nibble loses 6 when it has no carry out, the high nibble loses 0x60
/// when the intermediate fits in a byte — and there is no borrow
/// propagation between the corrections.  N and V latch from the
/// intermediate after the low-nibble correction and before the
/// high-nibble correction (as for decimal ADC); Z and C follow the
/// binary sum.  Derived empirically from TomHarte's SingleStepTests
/// 6502 data (verified over all distinct decimal-mode SBC cases).
pub fn sub8_dec_nmos(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let inv = !d & 0xFF;
    let bin: u16 = acc + inv + if carry { 1 } else { 0 };

    let mut c = if carry { 1 } else { 0 };
    let mut lo: u16 = (acc & 0x000F) + (inv & 0x000F) + c;
    // Carry out of the low nibble (before its correction).
    c = if lo > 0x000F { 1 } else { 0 };
    if lo <= 0x000F {
        lo = lo.wrapping_sub(0x0006);
    }
    let mut result = (acc & 0x00F0) + (inv & 0x00F0) + (c << 4) + (lo & 0x000F);
    let n = result & 0x80 != 0;
    let v = !(acc ^ inv) & (acc ^ result) & 0x80 != 0;
    if result <= 0x00FF {
        result = result.wrapping_sub(0x0060);
    }

    AluResult {
        result: result & 0xFF,
        z: (bin & 0xFF) == 0,
        n,
        v,
        c: bin > u8::MAX.into(),
    }
}

/// 8-bit add (decimal mode) with the NMOS 6502 flag quirks.
///
/// The result and carry follow the standard BCD algorithm, but the NMOS
/// 6502 latches N and V from the intermediate result *after* the
/// low-nibble correction and *before* the high-nibble correction, and
/// latches Z from the uncorrected binary sum.  Derived empirically from
/// TomHarte's SingleStepTests 6502 data.
pub fn add8_dec_nmos(acc: u16, d: u16, carry: bool) -> AluResult {
    assert!(acc <= u8::MAX.into());
    assert!(d <= u8::MAX.into());

    let bin: u16 = acc + d + if carry { 1 } else { 0 };

    let mut c = if carry { 1 } else { 0 };
    let mut lo: u16 = (acc & 0x000F) + (d & 0x000F) + c;
    if lo > 0x0009 {
        lo += 0x0006;
    }
    c = if lo > 0x000F { 1 } else { 0 };
    let mut result = (acc & 0x00F0) + (d & 0x00F0) + (c << 4) + (lo & 0x000F);
    let n = result & 0x80 != 0;
    let v = !(acc ^ d) & (acc ^ result) & 0x80 != 0;
    if result > 0x009F {
        result += 0x0060;
    }

    AluResult {
        result: result & 0xFF,
        z: (bin & 0xFF) == 0,
        n,
        v,
        c: result > u8::MAX.into(),
    }
}

/// 16-bit subtract (decimal mode)
pub fn sub16_dec(acc: u16, d: u16, carry: bool) -> AluResult {
    let acc = acc as i32;
    let d = !d as i32;
    let mut c = if carry { 1 } else { 0 };
    let mut result: i32 = (acc & 0x000F) + (d & 0x000F) + c;
    if result <= 0x000F {
        result -= 0x0006;
    }
    c = if result > 0x000F { 1 } else { 0 };
    result = (acc & 0x00F0) + (d & 0x00F0) + (c << 4) + (result & 0x000F);
    if result <= 0x00FF {
        result -= 0x0060;
    }

    c = if result > 0x00FF { 1 } else { 0 };
    result = (acc & 0x0F00) + (d & 0x0F00) + (c << 8) + (result & 0x00FF);
    if result <= 0x0FFF {
        result -= 0x0600;
    }
    c = if result > 0x0FFF { 1 } else { 0 };

    result = (acc & 0xF000) + (d & 0xF000) + (c << 12) + (result & 0x0FFF);
    let v = !(acc ^ d) & (acc ^ result) & 0x8000 != 0;
    if result <= 0xFFFF {
        result -= 0x6000;
    }

    AluResult {
        result: result as u16,
        z: result as u16 == 0,
        n: result & 0x8000 != 0,
        v,
        c: result > u16::MAX.into(),
    }
}
