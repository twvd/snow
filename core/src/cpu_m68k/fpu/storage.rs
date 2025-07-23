use anyhow::Result;
use arpfloat::{BigInt, Float};
use arrayvec::ArrayVec;
use proc_bitfield::bitfield;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::{cpu::CpuM68k, CpuM68kType};
use crate::types::{Long, Word};

use super::SEMANTICS_EXTENDED;

const EXPONENT_BIAS: u64 = 16383;
const EXPONENT_MAX: u64 = 0x7FFF;

pub const SINGLE_SIZE: usize = 4;
pub const DOUBLE_SIZE: usize = 8;
pub const EXTENDED_SIZE: usize = 12;

bitfield! {
    /// Raw (storage) bit representation of the extended-precision real format
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub struct BitsExtReal(pub u128): Debug, FromStorage, IntoStorage, DerefStorage {
        /// f (Mantissa)
        pub f: u64 @ 0..=62,

        /// i/j? (Explicit integer bit)
        pub i: bool @ 63,

        /// Raw mantissa (f + i)
        /// Where the implicit 1 in IEEE 754 is reused for the explicit integer bit
        pub raw_mantissa: u64 @ 0..=63,

        /// Zero
        pub z: u32 [read_only] @ 64..=79,

        /// e (Biased exponent)
        pub e: u64 @ 80..=94,

        /// s (Sign bit)
        pub s: bool @ 95,

        pub low: u32 @ 0..=31,
        pub mid: u32 @ 32..=63,
        pub high: u32 @ 64..=95,
    }
}

impl BitsExtReal {
    pub fn nan(s: bool) -> Self {
        // PRM 1.6.5
        Self::default()
            .with_e(u64::MAX)
            .with_f(u64::MAX)
            .with_s(s)
            .with_i(true)
    }

    pub fn is_nan(&self) -> bool {
        // PRM 1.6.5
        self.e() == EXPONENT_MAX && self.f() != 0
    }

    pub fn inf(s: bool) -> Self {
        // PRM 1.6.4
        Self::default()
            .with_e(u64::MAX)
            .with_f(0)
            .with_s(s)
            .with_i(true)
    }

    pub fn is_inf(&self) -> bool {
        // PRM 1.6.4
        self.e() == EXPONENT_MAX && self.f() == 0
    }

    pub fn zero(s: bool) -> Self {
        // PRM 1.6.3
        Self::default().with_e(0).with_f(0).with_s(s)
    }

    pub fn is_zero(&self) -> bool {
        // PRM 1.6.3
        self.e() == 0 && self.f() == 0
    }
}

impl From<&Float> for BitsExtReal {
    fn from(value: &Float) -> Self {
        if value.is_nan() {
            Self::nan(value.is_negative())
        } else if value.is_inf() {
            Self::inf(value.is_negative())
        } else if value.is_zero() {
            Self::zero(value.is_negative())
        } else {
            // Apply M68881 bias (16383) to the unbiased exponent from arpfloat
            let unbiased_exp = value.get_exp();
            let biased_exp = unbiased_exp + EXPONENT_BIAS as i64;

            // Ensure the biased exponent fits in 15 bits and is positive
            assert!(
                biased_exp >= 0,
                "Biased exponent {} is negative for unbiased exp {}",
                biased_exp,
                unbiased_exp
            );
            assert!(
                biased_exp < (1 << 15),
                "Biased exponent {} exceeds 15 bits for unbiased exp {}",
                biased_exp,
                unbiased_exp
            );

            Self::default()
                .with_s(value.is_negative())
                .with_raw_mantissa(value.get_mantissa().as_u64())
                .with_e(biased_exp as u64)
        }
    }
}

impl From<BitsExtReal> for Float {
    fn from(value: BitsExtReal) -> Self {
        if value.is_nan() {
            Self::nan(SEMANTICS_EXTENDED, value.s())
        } else if value.is_inf() {
            Self::inf(SEMANTICS_EXTENDED, value.s())
        } else if value.is_zero() {
            Self::zero(SEMANTICS_EXTENDED, value.s())
        } else {
            // Convert M68881 biased exponent back to unbiased for arpfloat
            let biased_exp = value.e() as i64;
            let unbiased_exp = biased_exp - EXPONENT_BIAS as i64;

            Self::from_parts(
                SEMANTICS_EXTENDED,
                value.s(),
                unbiased_exp,
                BigInt::from_u64(value.raw_mantissa()),
            )
        }
    }
}

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType, const PMMU: bool>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE, PMMU>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Read FPU extended precision value from memory
    pub(in crate::cpu_m68k) fn read_fpu_extended(&mut self, addr: Address) -> Result<Float> {
        // Extended precision format: 96 bits (12 bytes)
        // Read as 3 longs: sign/exponent (16 bits) + mantissa (64 bits)
        let high = self.read_ticks::<Long>(addr)?;
        let mid = self.read_ticks::<Long>(addr.wrapping_add(4))?;
        let low = self.read_ticks::<Long>(addr.wrapping_add(8))?;
        let bits = BitsExtReal::default()
            .with_low(low)
            .with_mid(mid)
            .with_high(high);

        Ok(bits.into())
    }

    /// Read FPU extended precision value immediate
    pub(in crate::cpu_m68k) fn read_fpu_extended_imm(&mut self) -> Result<Float> {
        // Extended precision format: 96 bits (12 bytes)
        // Read as 3 longs: sign/exponent (16 bits) + mantissa (64 bits)
        let high = self.fetch_immediate::<Long>()?;
        let mid = self.fetch_immediate::<Long>()?;
        let low = self.fetch_immediate::<Long>()?;
        let bits = BitsExtReal::default()
            .with_low(low)
            .with_mid(mid)
            .with_high(high);

        Ok(bits.into())
    }

    /// Write FPU extended precision value to memory
    pub(in crate::cpu_m68k) fn write_fpu_extended(
        &mut self,
        addr: Address,
        value: &Float,
    ) -> Result<()> {
        let bits = BitsExtReal::from(value);

        // Write as 3 longs
        self.write_ticks(addr, bits.high())?;
        self.write_ticks(addr.wrapping_add(4), bits.mid())?;
        self.write_ticks(addr.wrapping_add(8), bits.low())?;

        Ok(())
    }

    /// Read FPU double precision value from memory
    pub(in crate::cpu_m68k) fn read_fpu_double(&mut self, addr: Address) -> Result<Float> {
        let mut v = ArrayVec::<u8, 8>::new();

        for i in 0..8 {
            v.push(self.read_ticks(addr.wrapping_add(i))?);
        }
        Ok(Float::from_f64(f64::from_be_bytes(v.as_slice().try_into()?)).cast(SEMANTICS_EXTENDED))
    }

    /// Read FPU double precision value immediate
    pub(in crate::cpu_m68k) fn read_fpu_double_imm(&mut self) -> Result<Float> {
        let mut v = ArrayVec::<u8, 8>::new();

        for _ in 0..4 {
            let word = self.fetch_immediate::<Word>()?;
            v.try_extend_from_slice(&word.to_be_bytes())?;
        }
        Ok(Float::from_f64(f64::from_be_bytes(v.as_slice().try_into()?)).cast(SEMANTICS_EXTENDED))
    }

    /// Write FPU double precision value to memory
    pub(in crate::cpu_m68k) fn write_fpu_double(
        &mut self,
        addr: Address,
        value: &Float,
    ) -> Result<()> {
        for (i, b) in value.as_f64().to_be_bytes().into_iter().enumerate() {
            self.write_ticks(addr.wrapping_add(i as Address), b)?;
        }

        Ok(())
    }

    /// Read FPU single precision value from memory
    pub(in crate::cpu_m68k) fn read_fpu_single(&mut self, addr: Address) -> Result<Float> {
        let raw = self.read_ticks::<Long>(addr)?;
        Ok(Float::from_f32(f32::from_bits(raw)).cast(SEMANTICS_EXTENDED))
    }

    /// Read FPU single precision value from data register
    pub(in crate::cpu_m68k) fn read_fpu_single_dn(&self, dn: usize) -> Result<Float> {
        let raw = self.regs.read_d(dn);
        Ok(Float::from_f32(f32::from_bits(raw)).cast(SEMANTICS_EXTENDED))
    }

    /// Read FPU single precision value immediate
    pub(in crate::cpu_m68k) fn read_fpu_single_imm(&mut self) -> Result<Float> {
        let raw = self.fetch_immediate::<Long>()?;
        Ok(Float::from_f32(f32::from_bits(raw)).cast(SEMANTICS_EXTENDED))
    }

    /// Write FPU single precision value to memory
    pub(in crate::cpu_m68k) fn write_fpu_single(
        &mut self,
        addr: Address,
        value: &Float,
    ) -> Result<()> {
        self.write_ticks::<Long>(addr, value.as_f32().to_bits())
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::testbus::Testbus;
    use crate::bus::Address;
    use crate::cpu_m68k::{CpuM68020, M68020_ADDRESS_MASK};
    use crate::types::Byte;

    use super::*;
    use arpfloat::{BigInt, Float};

    // M68881 Extended Precision Constants
    const EXPONENT_MAX: u64 = 0x7FFF; // All 1s in 15-bit exponent (32767)
    const MANTISSA_EXPLICIT_BIT: u64 = 1u64 << 63; // Bit 63 - explicit integer bit
    const MANTISSA_FRACTION_MASK: u64 = (1u64 << 63) - 1; // Bits 0-62

    fn fully_equal(a: &Float, b: &Float) {
        kinda_equal(a, b);
        assert_eq!(a.get_mantissa(), b.get_mantissa());
        assert_eq!(a.get_exp(), b.get_exp());
    }

    fn kinda_equal(a: &Float, b: &Float) {
        assert_eq!(a.is_negative(), b.is_negative());
        assert_eq!(a.is_inf(), b.is_inf());
        assert_eq!(a.is_zero(), b.is_zero());
        assert_eq!(a.is_nan(), b.is_nan());
        if !a.is_nan() && !a.is_inf() {
            assert_eq!(a.as_f64(), b.as_f64());
        }
    }

    #[test]
    fn read_write_extended_real() {
        let values = vec![
            Float::zero(SEMANTICS_EXTENDED, false),
            Float::zero(SEMANTICS_EXTENDED, true),
            Float::from_u64(SEMANTICS_EXTENDED, 1234567890),
            Float::nan(SEMANTICS_EXTENDED, false),
            Float::nan(SEMANTICS_EXTENDED, true),
            Float::from_f64(3.1415).cast(SEMANTICS_EXTENDED),
            Float::from_f64(-3.1415).cast(SEMANTICS_EXTENDED),
        ];

        for v in values {
            eprintln!("Testing {} / {:?}", &v, &v);

            let mut cpu =
                CpuM68020::<Testbus<Address, Byte>>::new(Testbus::new(M68020_ADDRESS_MASK));

            // Ensure _something_ was written
            for a in 0..12 {
                cpu.write_ticks::<Byte>(a, 0xAA_u8).unwrap();
            }
            // Canary for writes too far
            cpu.write_ticks::<Long>(12, 0xDEADBEEF_u32).unwrap();
            cpu.write_fpu_extended(0, &v).unwrap();

            // Addresses should have been written to
            for a in 0..12 {
                assert_ne!(cpu.read_ticks::<Byte>(a).unwrap(), 0xAA_u8);
            }
            // Check canary
            assert_eq!(cpu.read_ticks::<Long>(12).unwrap(), 0xDEADBEEF_u32);

            // Check reading back actual value
            let read = cpu.read_fpu_extended(0).unwrap();
            eprintln!("Read back: {}, {:?}", &read, &read);
            fully_equal(&read, &v);
            assert_eq!(read.get_semantics(), SEMANTICS_EXTENDED);
        }
    }

    #[test]
    fn read_write_double_real() {
        let values = vec![
            Float::zero(SEMANTICS_EXTENDED, false),
            Float::zero(SEMANTICS_EXTENDED, true),
            Float::from_u64(SEMANTICS_EXTENDED, 1234567890),
            Float::nan(SEMANTICS_EXTENDED, false),
            Float::nan(SEMANTICS_EXTENDED, true),
            Float::from_f64(3.1415).cast(SEMANTICS_EXTENDED),
            Float::from_f64(-3.1415).cast(SEMANTICS_EXTENDED),
        ];

        for v in values {
            eprintln!("Testing {} / {:?}", &v, &v);

            let mut cpu =
                CpuM68020::<Testbus<Address, Byte>>::new(Testbus::new(M68020_ADDRESS_MASK));

            // Ensure _something_ was written
            for a in 0..8 {
                cpu.write_ticks::<Byte>(a, 0xAA_u8).unwrap();
            }
            // Canary for writes too far
            cpu.write_ticks::<Long>(8, 0xDEADBEEF_u32).unwrap();
            cpu.write_fpu_double(0, &v).unwrap();

            // Addresses should have been written to
            for a in 0..8 {
                assert_ne!(cpu.read_ticks::<Byte>(a).unwrap(), 0xAA_u8);
            }
            // Check canary
            assert_eq!(cpu.read_ticks::<Long>(8).unwrap(), 0xDEADBEEF_u32);

            // Check reading back actual value
            let read = cpu.read_fpu_double(0).unwrap();
            eprintln!("Read back: {}, {:?}", &read, &read);
            kinda_equal(&read, &v);
            assert_eq!(read.get_semantics(), SEMANTICS_EXTENDED);
        }
    }

    #[test]
    fn read_write_single_real() {
        let values = vec![
            Float::zero(SEMANTICS_EXTENDED, false),
            Float::zero(SEMANTICS_EXTENDED, true),
            Float::from_u64(SEMANTICS_EXTENDED, 12345678),
            Float::nan(SEMANTICS_EXTENDED, false),
            Float::nan(SEMANTICS_EXTENDED, true),
            Float::from_f32(3.14).cast(SEMANTICS_EXTENDED),
            Float::from_f32(-3.14).cast(SEMANTICS_EXTENDED),
        ];

        for v in values {
            eprintln!("Testing {} / {:?}", &v, &v);

            let mut cpu =
                CpuM68020::<Testbus<Address, Byte>>::new(Testbus::new(M68020_ADDRESS_MASK));

            // Ensure _something_ was written
            for a in 0..4 {
                cpu.write_ticks::<Byte>(a, 0xAA_u8).unwrap();
            }
            // Canary for writes too far
            cpu.write_ticks::<Long>(4, 0xDEADBEEF_u32).unwrap();
            cpu.write_fpu_single(0, &v).unwrap();

            // Addresses should have been written to
            for a in 0..4 {
                assert_ne!(cpu.read_ticks::<Byte>(a).unwrap(), 0xAA_u8);
            }
            // Check canary
            assert_eq!(cpu.read_ticks::<Long>(4).unwrap(), 0xDEADBEEF_u32);

            // Check reading back actual value
            let read = cpu.read_fpu_single(0).unwrap();
            eprintln!("Read back: {}, {:?}", &read, &read);
            kinda_equal(&read, &v);
            assert_eq!(read.get_semantics(), SEMANTICS_EXTENDED);
        }
    }

    #[test]
    fn test_exponent_bias_values() {
        // Test specific exponent bias calculations
        let test_cases = vec![
            // (unbiased_exponent, expected_biased_exponent)
            (0, 16383),     // Exponent 0 + bias 16383 = 16383
            (1, 16384),     // Exponent 1 + bias 16383 = 16384
            (-1, 16382),    // Exponent -1 + bias 16383 = 16382
            (100, 16483),   // Exponent 100 + bias 16383 = 16483
            (-100, 16283),  // Exponent -100 + bias 16383 = 16283
            (16383, 32766), // Max positive normal exponent (16383 + 16383 = 32766)
            (-16382, 1),    // Min positive normal exponent (-16382 + 16383 = 1)
        ];

        for (unbiased_exp, expected_biased) in test_cases {
            // Create a normalized number with explicit integer bit set
            let mantissa = BigInt::from_u64(MANTISSA_EXPLICIT_BIT | (1u64 << 62)); // 1.1xxx...
            let float_val = Float::from_parts(
                SEMANTICS_EXTENDED,
                false,        // positive
                unbiased_exp, // This is the unbiased exponent arpfloat expects
                mantissa,
            );

            let bits = BitsExtReal::from(&float_val);

            assert_eq!(
                bits.e() as i64,
                expected_biased as i64,
                "Exponent bias incorrect for unbiased exponent {}: got {}, expected {}",
                unbiased_exp,
                bits.e(),
                expected_biased
            );

            // Verify integer bit is set for normal numbers
            if unbiased_exp >= -16382 {
                // Normal range
                assert_eq!(
                    bits.i(),
                    true,
                    "Integer bit should be set for normal number with exponent {}",
                    unbiased_exp
                );
            }

            println!(
                "Bias test: unbiased={}, biased={} ✓",
                unbiased_exp, expected_biased
            );
        }
    }

    #[test]
    fn test_normalization_explicit_integer_bit() {
        // Test that the explicit integer bit (bit 63) is correctly handled

        // Normal number: exponent != 0, integer bit = 1
        let normal_float = Float::from_f64(1.5).cast(SEMANTICS_EXTENDED);
        let normal_bits = BitsExtReal::from(&normal_float);

        assert_ne!(
            normal_bits.e(),
            0,
            "Normal number should have non-zero exponent"
        );
        assert_eq!(
            normal_bits.i(),
            true,
            "Normal number should have integer bit set"
        );
        assert_ne!(
            normal_bits.f(),
            0,
            "Normal number should have non-zero fractional mantissa"
        );

        // Verify the mantissa includes the explicit integer bit
        let expected_mantissa = normal_bits.raw_mantissa();
        assert_eq!(
            expected_mantissa & MANTISSA_EXPLICIT_BIT,
            MANTISSA_EXPLICIT_BIT,
            "Raw mantissa should include explicit integer bit"
        );
    }

    #[test]
    fn test_denormal_numbers() {
        // Create a denormal number (exponent = 0, integer bit = 0)
        // Denormal numbers have the form 0.fraction * 2^(1-bias)

        // For M68881, denormal numbers should have biased exponent = 0
        // This corresponds to unbiased exponent = 0 - 16383 = -16383
        let denormal_mantissa = BigInt::from_u64(1u64 << 61); // 0.01xxx... (no integer bit)
        let denormal_float = Float::from_parts(
            SEMANTICS_EXTENDED,
            false,  // positive
            -16383, // This should result in stored exponent = 0
            denormal_mantissa,
        );

        let denormal_bits = BitsExtReal::from(&denormal_float);

        println!(
            "Denormal test: unbiased_exp={}, biased_exp={}, integer_bit={}, f=0x{:X}",
            denormal_float.get_exp(),
            denormal_bits.e(),
            denormal_bits.i(),
            denormal_bits.f()
        );

        // For denormal numbers in M68881:
        // - Stored exponent = 0
        // - Integer bit (bit 63) = 0
        // - Fractional part != 0
        assert_eq!(
            denormal_bits.e(),
            0,
            "Denormal number should have zero exponent"
        );
        // Note: arpfloat may normalize this, so we need to check what actually happens

        if denormal_bits.e() == 0 {
            // Only check integer bit if we actually got a denormal
            assert_eq!(
                denormal_bits.i(),
                false,
                "Denormal number should have integer bit clear"
            );
            assert_ne!(
                denormal_bits.f(),
                0,
                "Denormal number should have non-zero fractional mantissa"
            );
        } else {
            println!("Note: arpfloat normalized what we expected to be denormal");
        }
    }

    #[test]
    fn test_zero_representation() {
        // Test positive and negative zero
        let pos_zero = Float::zero(SEMANTICS_EXTENDED, false);
        let neg_zero = Float::zero(SEMANTICS_EXTENDED, true);

        let pos_zero_bits = BitsExtReal::from(&pos_zero);
        let neg_zero_bits = BitsExtReal::from(&neg_zero);

        // Both zeros should have:
        // - Exponent = 0
        // - Integer bit = 0
        // - Fractional mantissa = 0
        assert_eq!(
            pos_zero_bits.e(),
            0,
            "Positive zero should have zero exponent"
        );
        assert_eq!(
            pos_zero_bits.i(),
            false,
            "Positive zero should have integer bit clear"
        );
        assert_eq!(
            pos_zero_bits.f(),
            0,
            "Positive zero should have zero fractional mantissa"
        );
        assert_eq!(
            pos_zero_bits.s(),
            false,
            "Positive zero should have sign bit clear"
        );

        assert_eq!(
            neg_zero_bits.e(),
            0,
            "Negative zero should have zero exponent"
        );
        assert_eq!(
            neg_zero_bits.i(),
            false,
            "Negative zero should have integer bit clear"
        );
        assert_eq!(
            neg_zero_bits.f(),
            0,
            "Negative zero should have zero fractional mantissa"
        );
        assert_eq!(
            neg_zero_bits.s(),
            true,
            "Negative zero should have sign bit set"
        );
    }

    #[test]
    fn test_infinity_representation() {
        // Test positive and negative infinity
        let pos_inf = Float::inf(SEMANTICS_EXTENDED, false);
        let neg_inf = Float::inf(SEMANTICS_EXTENDED, true);

        let pos_inf_bits = BitsExtReal::from(&pos_inf);
        let neg_inf_bits = BitsExtReal::from(&neg_inf);

        println!(
            "Infinity test: pos_inf biased_exp={}, neg_inf biased_exp={}",
            pos_inf_bits.e(),
            neg_inf_bits.e()
        );

        // Both infinities should have:
        // - Exponent = all 1s (0x7FFF = 32767)
        // - Integer bit = 1 (for M68881)
        // - Fractional mantissa = 0
        assert_eq!(
            pos_inf_bits.e(),
            EXPONENT_MAX,
            "Positive infinity should have max exponent ({})",
            EXPONENT_MAX
        );
        assert_eq!(
            pos_inf_bits.i(),
            true,
            "Positive infinity should have integer bit set"
        );
        assert_eq!(
            pos_inf_bits.f(),
            0,
            "Positive infinity should have zero fractional mantissa"
        );
        assert_eq!(
            pos_inf_bits.s(),
            false,
            "Positive infinity should have sign bit clear"
        );

        assert_eq!(
            neg_inf_bits.e(),
            EXPONENT_MAX,
            "Negative infinity should have max exponent ({})",
            EXPONENT_MAX
        );
        assert_eq!(
            neg_inf_bits.i(),
            true,
            "Negative infinity should have integer bit set"
        );
        assert_eq!(
            neg_inf_bits.f(),
            0,
            "Negative infinity should have zero fractional mantissa"
        );
        assert_eq!(
            neg_inf_bits.s(),
            true,
            "Negative infinity should have sign bit set"
        );
    }

    #[test]
    fn test_nan_representation() {
        // Test NaN representation
        let nan_pos = Float::nan(SEMANTICS_EXTENDED, false);
        let nan_neg = Float::nan(SEMANTICS_EXTENDED, true);

        let nan_pos_bits = BitsExtReal::from(&nan_pos);
        let nan_neg_bits = BitsExtReal::from(&nan_neg);

        println!(
            "NaN test: pos_nan biased_exp={}, f=0x{:X}; neg_nan biased_exp={}, f=0x{:X}",
            nan_pos_bits.e(),
            nan_pos_bits.f(),
            nan_neg_bits.e(),
            nan_neg_bits.f()
        );

        // Both NaNs should have:
        // - Exponent = all 1s (0x7FFF = 32767)
        // - Integer bit = 1 (for M68881)
        // - Fractional mantissa != 0
        assert_eq!(
            nan_pos_bits.e(),
            EXPONENT_MAX,
            "Positive NaN should have max exponent ({})",
            EXPONENT_MAX
        );
        assert_eq!(
            nan_pos_bits.i(),
            true,
            "Positive NaN should have integer bit set"
        );
        assert_ne!(
            nan_pos_bits.f(),
            0,
            "Positive NaN should have non-zero fractional mantissa"
        );
        assert_eq!(
            nan_pos_bits.s(),
            false,
            "Positive NaN should have sign bit clear"
        );

        assert_eq!(
            nan_neg_bits.e(),
            EXPONENT_MAX,
            "Negative NaN should have max exponent ({})",
            EXPONENT_MAX
        );
        assert_eq!(
            nan_neg_bits.i(),
            true,
            "Negative NaN should have integer bit set"
        );
        assert_ne!(
            nan_neg_bits.f(),
            0,
            "Negative NaN should have non-zero fractional mantissa"
        );
        assert_eq!(
            nan_neg_bits.s(),
            true,
            "Negative NaN should have sign bit set"
        );
    }

    #[test]
    fn test_boundary_exponents() {
        // Test minimum and maximum normal exponents

        // Minimum normal exponent: unbiased=-16382, biased=1 (since -16382 + 16383 = 1)
        let min_normal_mantissa = BigInt::from_u64(MANTISSA_EXPLICIT_BIT); // 1.0
        let min_normal = Float::from_parts(
            SEMANTICS_EXTENDED,
            false,
            -16382, // Unbiased exponent
            min_normal_mantissa,
        );
        let min_bits = BitsExtReal::from(&min_normal);

        assert_eq!(
            min_bits.e(),
            1,
            "Minimum normal should have stored exponent 1"
        );
        assert_eq!(
            min_bits.i(),
            true,
            "Minimum normal should have integer bit set"
        );

        // Maximum normal exponent: unbiased=16383, biased=32766 (since 16383 + 16383 = 32766)
        // Note: 32767 (0x7FFF) is reserved for infinity/NaN
        let max_normal_mantissa = BigInt::from_u64(MANTISSA_EXPLICIT_BIT | MANTISSA_FRACTION_MASK); // 1.111...
        let max_normal = Float::from_parts(
            SEMANTICS_EXTENDED,
            false,
            16383, // Unbiased exponent
            max_normal_mantissa,
        );
        let max_bits = BitsExtReal::from(&max_normal);

        assert_eq!(
            max_bits.e(),
            32766,
            "Maximum normal should have stored exponent 32766"
        );
        assert_eq!(
            max_bits.i(),
            true,
            "Maximum normal should have integer bit set"
        );

        println!(
            "Boundary test: min_normal biased={}, max_normal biased={}",
            min_bits.e(),
            max_bits.e()
        );
    }

    #[test]
    fn test_round_trip_normalization() {
        // Test that normalization is preserved through round-trip conversion
        let test_values = vec![
            1.0f64,
            -1.0f64,
            2.0f64,
            0.5f64,
            1.5f64,
            std::f64::consts::PI,
            std::f64::consts::E,
            1e-100f64,
            1e100f64,
        ];

        for val in test_values {
            let original = Float::from_f64(val).cast(SEMANTICS_EXTENDED);
            let bits = BitsExtReal::from(&original);
            let recovered = Float::from(bits);

            // Verify that normalization properties are preserved
            if !original.is_zero() && !original.is_inf() && !original.is_nan() {
                // Normal numbers should have integer bit set
                assert_eq!(
                    bits.i(),
                    true,
                    "Normal number {} should have integer bit set",
                    val
                );
                assert_ne!(
                    bits.e(),
                    0,
                    "Normal number {} should have non-zero exponent",
                    val
                );
                assert_ne!(
                    bits.e(),
                    EXPONENT_MAX,
                    "Normal number {} should not have max exponent",
                    val
                );
            }

            // Verify round-trip accuracy
            assert_eq!(
                original.is_negative(),
                recovered.is_negative(),
                "Sign should be preserved for {}",
                val
            );
            assert_eq!(
                original.is_zero(),
                recovered.is_zero(),
                "Zero property should be preserved for {}",
                val
            );
            assert_eq!(
                original.is_inf(),
                recovered.is_inf(),
                "Infinity property should be preserved for {}",
                val
            );
            assert_eq!(
                original.is_nan(),
                recovered.is_nan(),
                "NaN property should be preserved for {}",
                val
            );
        }
    }

    #[test]
    fn test_memory_bias_persistence() {
        // Test that bias is correctly preserved when storing/loading from memory
        let mut cpu = CpuM68020::<Testbus<Address, Byte>>::new(Testbus::new(M68020_ADDRESS_MASK));

        let test_exponents = vec![-1000, -1, 0, 1, 1000];

        for exp in test_exponents {
            let mantissa = BigInt::from_u64(MANTISSA_EXPLICIT_BIT | (0x1234567890ABCDEFu64 >> 1));
            let original = Float::from_parts(SEMANTICS_EXTENDED, false, exp, mantissa);

            println!(
                "Memory test for unbiased exp {}: arpfloat get_exp()={}",
                exp,
                original.get_exp()
            );

            // Store to memory and read back
            cpu.write_fpu_extended(0, &original).unwrap();
            let recovered = cpu.read_fpu_extended(0).unwrap();

            // Verify exponent bias is preserved
            assert_eq!(
                original.get_exp(),
                recovered.get_exp(),
                "Exponent should be preserved through memory for exp {}",
                exp
            );

            // Verify normalization is preserved
            let original_bits = BitsExtReal::from(&original);
            let recovered_bits = BitsExtReal::from(&recovered);

            assert_eq!(
                original_bits.i(),
                recovered_bits.i(),
                "Integer bit should be preserved through memory for exp {}",
                exp
            );
            assert_eq!(
                original_bits.e(),
                recovered_bits.e(),
                "Stored exponent should be preserved through memory for exp {}",
                exp
            );

            println!(
                "  -> biased exp: original={}, recovered={} ✓",
                original_bits.e(),
                recovered_bits.e()
            );
        }
    }

    #[test]
    fn test_special_normalization_cases() {
        // Test edge cases in normalization

        // Test smallest positive normal number: unbiased exp -16382, biased exp 1
        let smallest_normal_mantissa = BigInt::from_u64(MANTISSA_EXPLICIT_BIT); // 1.0
        let smallest_normal = Float::from_parts(
            SEMANTICS_EXTENDED,
            false,
            -16382, // Minimum normal unbiased exponent
            smallest_normal_mantissa,
        );
        let bits = BitsExtReal::from(&smallest_normal);

        assert_eq!(bits.e(), 1, "Smallest normal should have stored exponent 1");
        assert_eq!(
            bits.i(),
            true,
            "Smallest normal should have integer bit set"
        );
        assert_eq!(
            bits.f(),
            0,
            "Smallest normal should have zero fractional part"
        );

        // Test largest finite number: unbiased exp 16383, biased exp 32766
        let largest_mantissa = BigInt::from_u64(MANTISSA_EXPLICIT_BIT | MANTISSA_FRACTION_MASK);
        let largest_finite = Float::from_parts(
            SEMANTICS_EXTENDED,
            false,
            16383, // Maximum normal unbiased exponent (32767 reserved for inf/nan)
            largest_mantissa,
        );
        let largest_bits = BitsExtReal::from(&largest_finite);

        assert_eq!(
            largest_bits.e(),
            32766,
            "Largest finite should have stored exponent 32766"
        );
        assert_eq!(
            largest_bits.i(),
            true,
            "Largest finite should have integer bit set"
        );
        assert_eq!(
            largest_bits.f(),
            MANTISSA_FRACTION_MASK,
            "Largest finite should have all fractional bits set"
        );

        println!(
            "Special cases: smallest_normal biased={}, largest_finite biased={}",
            bits.e(),
            largest_bits.e()
        );
    }
}
