use crate::bus::Address;
use crate::cpu_m68k::fpu::storage::float_array_as_ext_real;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::types::{Byte, Long};
use arpfloat::Float;
use num_traits::Zero;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

bitfield! {
    /// Exception bitfields
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct FpuExceptions(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Inexact Decimal Input
        pub inex1: bool @ 0,

        /// Inexact Operation
        pub inex2: bool @ 1,

        /// Division by zero
        pub dz: bool @ 2,

        /// Underflow
        pub unfl: bool @ 3,

        /// Overflow
        pub ovfl: bool @ 4,

        /// Operand error
        pub operr: bool @ 5,

        /// Signaling Not-a-Number
        pub snan: bool @ 6,

        /// Branch/set on unordered
        pub bsun: bool @ 7,
    }
}

bitfield! {
    /// Accrued exception bitfields
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct FpuAccruedExceptions(pub u8): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Inexact
        pub inex: bool @ 3,

        /// Division by zero
        pub dz: bool @ 4,

        /// Underflow
        pub unfl: bool @ 5,

        /// Overflow
        pub ovfl: bool @ 6,

        /// Invalid operation
        pub iop: bool @ 7,

        /// Signaling Not-a-Number
        pub snan: bool @ 6,

        /// Branch/set on unordered
        pub bsun: bool @ 7,
    }
}

bitfield! {
    /// Floating Point Control Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RegisterFPCR(pub Long): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Full mode control byte
        pub mode: Byte @ 0..=7,

        /// Rounding mode
        pub rnd: u8 @ 4..=5,

        /// Rounding precision
        pub prec: u8 @ 6..=7,

        /// Exception control
        pub exc: nested FpuExceptions @ 8..=15,
    }
}

bitfield! {
    /// Floating Point Status Register
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RegisterFPSR(pub Long): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Full condition code byte
        pub fpcc: u8 @ 24..=31,

        /// Condition code: Not-a-number or unordered
        pub fpcc_nan: bool @ 24,

        /// Condition code: Infinity
        pub fpcc_i: bool @ 25,

        /// Condition code: Zero
        pub fpcc_z: bool @ 26,

        /// Condition code: Negative
        pub fpcc_n: bool @ 27,

        /// 7 least significant bits of quotient
        pub quotient: u8 @ 16..=22,

        /// Sign of quotient
        pub quotient_s: bool @ 23,

        /// Full exception status
        pub exs: nested FpuExceptions @ 8..=15,

        /// Accrued exception byte
        pub aexc: nested FpuAccruedExceptions @ 0..=7,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpuRegisterFile {
    #[serde(with = "float_array_as_ext_real")]
    pub fp: [Float; 8],
    pub fpcr: RegisterFPCR,
    pub fpsr: RegisterFPSR,
    pub fpiar: Address,
}

impl FpuRegisterFile {
    /// Creates a string with differences between this FpuRegisterFile and another
    pub fn diff_str(&self, other: &Self) -> String {
        let diff = |name, s, o| {
            if s != o {
                format!("{}: {:08X} -> {:08X} ", name, s, o)
            } else {
                String::new()
            }
        };
        let diff_fp = |name, s: &Float, o: &Float| {
            if s.is_inf() != o.is_inf()
                || s.is_nan() != o.is_nan()
                || (!s.is_nan() && !s.is_inf() && s.as_f64() != o.as_f64())
            {
                format!("{}: {:0.4} -> {:0.4} ", name, s.as_f64(), o.as_f64())
            } else {
                String::new()
            }
        };
        let mut out = String::new();
        for i in 0..8 {
            out.push_str(diff_fp(format!("FP{}", i), &self.fp[i], &other.fp[i]).as_str());
        }
        out.push_str(diff("FPCR".to_string(), self.fpcr.0, other.fpcr.0).as_str());
        out.push_str(diff("FPSR".to_string(), self.fpsr.0, other.fpsr.0).as_str());
        out
    }
}

impl Default for FpuRegisterFile {
    fn default() -> Self {
        Self {
            fp: core::array::from_fn(|_| Float::nan(SEMANTICS_EXTENDED, false)),
            fpcr: RegisterFPCR(0),
            fpsr: RegisterFPSR(0),
            fpiar: Address::zero(),
        }
    }
}

impl Eq for FpuRegisterFile {}
impl PartialEq for FpuRegisterFile {
    fn eq(&self, _other: &Self) -> bool {
        // TODO
        true
    }
}
