use anyhow::Result;
use arpfloat::{BigInt, Float};
use proc_bitfield::bitfield;

use crate::bus::{Address, Bus, IrqSource};
use crate::cpu_m68k::{cpu::CpuM68k, CpuM68kType};
use crate::types::Long;

use super::SEMANTICS_EXTENDED;

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
        Self::default().with_e(u64::MAX).with_f(u64::MAX).with_s(s)
    }

    pub fn is_nan(&self) -> bool {
        // PRM 1.6.5
        self.e() == ((1 << 15) - 1) && self.f() != 0
    }

    pub fn inf(s: bool) -> Self {
        // PRM 1.6.4
        Self::default().with_e(u64::MAX).with_f(0).with_s(s)
    }

    pub fn is_inf(&self) -> bool {
        // PRM 1.6.4
        self.e() == ((1 << 15) - 1) && self.f() == 0
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
            Self::default()
                .with_s(value.is_negative())
                .with_raw_mantissa(value.get_mantissa().as_u64())
                .with_e(value.get_exp() as u64)
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
            Self::from_parts(
                SEMANTICS_EXTENDED,
                value.s(),
                value.e() as i64,
                BigInt::from_u64(value.raw_mantissa()),
            )
        }
    }
}

impl<TBus, const ADDRESS_MASK: Address, const CPU_TYPE: CpuM68kType>
    CpuM68k<TBus, ADDRESS_MASK, CPU_TYPE>
where
    TBus: Bus<Address, u8> + IrqSource,
{
    /// Read FPU extended precision value from memory
    pub(in crate::cpu_m68k) fn read_fpu_extended(&mut self, addr: Address) -> Result<Float> {
        // Extended precision format: 96 bits (12 bytes)
        // Read as 3 longs: sign/exponent (16 bits) + mantissa (64 bits)
        let high = self.read_ticks::<Long>(addr)?;
        let mid = self.read_ticks::<Long>(addr + 4)?;
        let low = self.read_ticks::<Long>(addr + 8)?;
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
        self.write_ticks(addr + 4, bits.mid())?;
        self.write_ticks(addr + 8, bits.low())?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::testbus::Testbus;
    use crate::bus::Address;
    use crate::cpu_m68k::{CpuM68020, M68020_ADDRESS_MASK};
    use crate::types::Byte;

    use super::*;

    fn kinda_equal(a: &Float, b: &Float) {
        assert_eq!(a.is_negative(), b.is_negative());
        assert_eq!(a.get_mantissa(), b.get_mantissa());
        assert_eq!(a.get_exp(), b.get_exp());
        assert_eq!(a.is_inf(), b.is_inf());
        assert_eq!(a.is_zero(), b.is_zero());
        assert_eq!(a.is_nan(), b.is_nan());
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
            kinda_equal(&read, &v);
        }
    }
}
