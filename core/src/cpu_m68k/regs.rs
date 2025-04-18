use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use super::CpuSized;
use crate::bus::Address;
use crate::types::Long;

use std::fmt;

/// Generalization of an address/data register
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Register {
    Dn(usize),
    An(usize),
    USP,
    SSP,
    PC,
    SR,
}

impl std::fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dn(n) => write!(f, "D{}", n),
            Self::An(n) => write!(f, "A{}", n),
            Self::USP => write!(f, "USP"),
            Self::SSP => write!(f, "SSP"),
            Self::PC => write!(f, "PC"),
            Self::SR => write!(f, "SR"),
        }
    }
}

bitfield! {
    /// SR register bitfield
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RegisterSR(pub u16): Debug, FromRaw, IntoRaw, DerefRaw {
        /// Full SR (with masking)
        pub sr: u16 [set_fn (|v| v & 0b1010011100011111)] @ ..,
        /// Condition Code Register
        pub ccr: u8 @ 0..=4,
        /// Carry
        pub c: bool @ 0, // 1
        /// Overflow
        pub v: bool @ 1, // 2
        /// Zero
        pub z: bool @ 2, // 4
        /// Negative
        pub n: bool @ 3, // 8
        /// Extend
        pub x: bool @ 4, // 10

        /// Interrupt priority mask
        pub int_prio_mask: u8 @ 8..=10,

        /// Supervisor mode
        pub supervisor: bool @ 13,

        /// Trace mode
        pub trace: bool @ 15,
    }
}

/// Full Motorola 680x0 register file
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct RegisterFile {
    /// Dx
    pub d: [Long; 8],

    /// Ax
    pub a: [Long; 7],

    /// User Stack Pointer
    pub usp: Address,

    /// Supervisor Stack Pointer
    pub ssp: Address,

    /// Status Register
    pub sr: RegisterSR,

    /// Program counter
    pub pc: Address,
}

impl RegisterFile {
    pub fn new() -> Self {
        Self {
            a: [0; 7],
            d: [0; 8],
            usp: 0,
            ssp: 0,
            sr: RegisterSR(0),
            pc: 0,
        }
    }

    /// Creates a string with differences between this RegisterFile and another
    pub fn diff_str(&self, other: &Self) -> String {
        let diff = |name, s, o| {
            if s != o {
                format!("{}: {:08X} -> {:08X} ", name, s, o)
            } else {
                String::new()
            }
        };
        let diff_flag = |name, s, o| {
            if s != o {
                format!(
                    "SR.{}: {} -> {} ",
                    name,
                    if s { "1" } else { "0" },
                    if o { "1" } else { "0" }
                )
            } else {
                String::new()
            }
        };
        let mut out = String::new();
        for i in 0..8 {
            out.push_str(diff(format!("D{}", i), self.d[i], other.d[i]).as_str());
        }
        for i in 0..7 {
            out.push_str(diff(format!("A{}", i), self.a[i], other.a[i]).as_str());
        }
        out.push_str(diff("USP".to_string(), self.usp, other.usp).as_str());
        out.push_str(diff("SSP".to_string(), self.ssp, other.ssp).as_str());
        // PC skipped

        out.push_str(&diff_flag("C", self.sr.c(), other.sr.c()));
        out.push_str(&diff_flag("N", self.sr.n(), other.sr.n()));
        out.push_str(&diff_flag("V", self.sr.v(), other.sr.v()));
        out.push_str(&diff_flag("Z", self.sr.z(), other.sr.z()));
        out.push_str(&diff_flag("X", self.sr.x(), other.sr.x()));
        out.push_str(&diff_flag(
            "SV",
            self.sr.supervisor(),
            other.sr.supervisor(),
        ));
        out.push_str(&diff_flag("TRACE", self.sr.trace(), other.sr.trace()));
        out.push_str(
            diff(
                "SR.INTPRI".to_string(),
                self.sr.int_prio_mask().into(),
                other.sr.int_prio_mask().into(),
            )
            .as_str(),
        );
        out
    }

    /// Read an An register
    pub fn read_a<T: CpuSized>(&self, a: usize) -> T {
        T::chop(if a == 7 {
            if self.sr.supervisor() {
                self.ssp
            } else {
                self.usp
            }
        } else {
            self.a[a]
        })
    }

    /// Read an An register and post-increment
    pub fn read_a_postinc<T: CpuSized>(&mut self, a: usize, adjust: usize) -> T {
        let adjust = adjust as Long;

        T::chop(if a == 7 {
            // Byte also adjusts by 2 to keep the stack aligned
            let adjust = std::cmp::max(2, adjust);

            if self.sr.supervisor() {
                let result = self.ssp;
                self.ssp = self.ssp.wrapping_add(adjust);
                result
            } else {
                let result = self.usp;
                self.usp = self.usp.wrapping_add(adjust);
                result
            }
        } else {
            let result = self.a[a];
            self.a[a] = self.a[a].wrapping_add(adjust);
            result
        })
    }

    /// Read an An register and pre-decrement
    pub fn read_a_predec<T: CpuSized>(&mut self, a: usize, adjust: usize) -> T {
        let adjust = adjust as Long;

        T::chop(if a == 7 {
            // Byte also adjusts by 2 to keep the stack aligned
            let adjust = std::cmp::max(2, adjust);

            if self.sr.supervisor() {
                self.ssp = self.ssp.wrapping_sub(adjust);
                self.ssp
            } else {
                self.usp = self.usp.wrapping_sub(adjust);
                self.usp
            }
        } else {
            self.a[a] = self.a[a].wrapping_sub(adjust);
            self.a[a]
        })
    }

    /// Write an An register
    pub fn write_a<T: CpuSized>(&mut self, a: usize, val: T) {
        // Writes to A as Byte or Word are sign extended
        let adj_val = val.expand_sign_extend();

        if a == 7 {
            if self.sr.supervisor() {
                self.ssp = adj_val;
            } else {
                self.usp = adj_val;
            }
        } else {
            self.a[a] = adj_val;
        }
    }

    /// Write an An register, lower word only if not full width
    pub fn write_a_low<T: CpuSized>(&mut self, a: usize, val: T) {
        match std::mem::size_of::<T>() {
            // 1 is illegal
            2 => {
                let old: Long = self.read_a(a);
                let val = old & 0xFFFF0000 | val.expand();
                self.write_a(a, val);
            }
            4 => self.write_a(a, val),
            _ => unreachable!(),
        }
    }

    /// Read a Dn register
    pub fn read_d<T: CpuSized>(&self, d: usize) -> T {
        T::chop(self.d[d])
    }

    /// Write a Dn register
    pub fn write_d<T: CpuSized>(&mut self, d: usize, val: T) {
        self.d[d] = val.replace_in(self.d[d]);
    }

    /// Write a register, specifying a Register type
    pub fn write<T: CpuSized>(&mut self, reg: Register, value: T) {
        match reg {
            Register::An(r) => self.write_a(r, value),
            Register::Dn(r) => self.write_d(r, value),
            Register::USP => self.usp = value.expand(),
            Register::SSP => self.ssp = value.expand(),
            Register::PC => panic!("Must be written through CpuM68k::set_pc"),
            Register::SR => self.sr.set_sr(value.expand() as u16),
        }
    }

    /// Read a register, specifying a Register type
    pub fn read<T: CpuSized>(&self, reg: Register) -> T {
        match reg {
            Register::An(r) => self.read_a(r),
            Register::Dn(r) => self.read_d(r),
            Register::USP => T::chop(self.usp),
            Register::SSP => T::chop(self.ssp),
            Register::PC => T::chop(self.pc),
            Register::SR => T::chop(self.sr.sr().into()),
        }
    }
}

impl fmt::Display for RegisterFile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "A: {:X?} D: {:X?} USP: {:06X} SSP: {:06X} PC: {:06X} SR: {:X?}",
            self.a, self.d, self.usp, self.ssp, self.pc, self.sr
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Byte, Long, Word};

    #[test]
    fn read_d() {
        let mut r = RegisterFile::new();
        r.d[0] = 0x11223344;

        assert_eq!(r.read_d::<Byte>(0), 0x44);
        assert_eq!(r.read_d::<Word>(0), 0x3344);
        assert_eq!(r.read_d::<Long>(0), 0x11223344);
    }

    #[test]
    fn read_a() {
        let mut r = RegisterFile::new();
        r.a[0] = 0x11223344;

        assert_eq!(r.read_a::<Byte>(0), 0x44);
        assert_eq!(r.read_a::<Word>(0), 0x3344);
        assert_eq!(r.read_a::<Long>(0), 0x11223344);
    }

    #[test]
    fn write_a() {
        let mut r = RegisterFile::new();
        r.write_a(0, 0x11223344_u32);
        assert_eq!(r.a[0], 0x11223344);
        r.write_a(0, 0x3344_u16);
        assert_eq!(r.a[0], 0x00003344);
        r.write_a(0, 0x44_u8);
        assert_eq!(r.a[0], 0x00000044);
        r.write_a(0, 0xB344_u16);
        assert_eq!(r.a[0], 0xFFFFB344);
        r.write_a(0, 0xB4_u8);
        assert_eq!(r.a[0], 0xFFFFFFB4);
    }

    #[test]
    fn write_a7_user() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(false);
        r.ssp = 0;
        r.usp = 0;

        r.write_a(7, 0x11223344_u32);
        assert_eq!(r.usp, 0x11223344);
        assert_eq!(r.ssp, 0x00000000);
    }

    #[test]
    fn write_a7_supervisor() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(true);
        r.ssp = 0;
        r.usp = 0;

        r.write_a(7, 0x11223344_u32);
        assert_eq!(r.ssp, 0x11223344);
        assert_eq!(r.usp, 0x00000000);
    }

    #[test]
    fn read_a7_user() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(false);
        r.ssp = 0;
        r.usp = 0x11223344;

        assert_eq!(r.read_a::<Long>(7), 0x11223344_u32);
    }

    #[test]
    fn read_a7_supervisor() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(true);
        r.usp = 0;
        r.ssp = 0x11223344;

        assert_eq!(r.read_a::<Long>(7), 0x11223344_u32);
    }
}
