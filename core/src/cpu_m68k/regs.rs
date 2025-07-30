use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};

use super::CpuSized;
use crate::bus::Address;
use crate::cpu_m68k::fpu::regs::FpuRegisterFile;
use crate::cpu_m68k::pmmu::regs::PmmuRegisterFile;
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
    // M68010
    DFC,
    SFC,
    VBR,
    // M68020
    CAAR,
    CACR,
    MSP,
    ISP,
    // FPU
    FPCR,
    FPSR,
    FPIAR,
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
            Self::DFC => write!(f, "DFC"),
            Self::SFC => write!(f, "SFC"),
            Self::VBR => write!(f, "VBR"),
            Self::CAAR => write!(f, "CAAR"),
            Self::CACR => write!(f, "CACR"),
            Self::MSP => write!(f, "MSP"),
            Self::ISP => write!(f, "ISP"),
            Self::FPCR => write!(f, "FPCR"),
            Self::FPSR => write!(f, "FPSR"),
            Self::FPIAR => write!(f, "FPIAR"),
        }
    }
}

bitfield! {
    /// SR register bitfield
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RegisterSR(pub u16): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Full SR (with masking)
        pub sr: u16 [set_fn (|v| v & 0b1011011100011111)] @ ..,
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

        /// Master/Interrupt stack bit
        /// 0 = ISP, 1 = MSP
        /// 68020+ only
        pub m: bool @ 12,

        /// Supervisor mode
        pub supervisor: bool @ 13,

        /// Trace mode
        pub trace: bool @ 15,
    }
}

bitfield! {
    /// CACR register bitfield
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct RegisterCACR(pub Long): Debug, FromStorage, IntoStorage, DerefStorage {
        /// Cache enable
        pub e: bool @ 0,
        /// Freeze cache
        pub f: bool @ 1,
        /// Clear Entry In Cache
        pub ce: bool @ 2,
        /// Clear cache
        pub c: bool @ 3,
    }
}

/// Full Motorola 680x0 register file
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, Default)]
pub struct RegisterFile {
    /// Dx
    pub d: [Long; 8],

    /// Ax
    pub a: [Long; 7],

    /// User Stack Pointer
    pub usp: Address,

    /// Supervisor Stack Pointer (68000) / Interrupt Stack Pointer (68020+)
    pub isp: Address,

    /// Status Register
    pub sr: RegisterSR,

    /// Program counter
    pub pc: Address,

    /// Destination Function Code (68010+)
    pub dfc: Long,

    /// Source Function Code (68010+)
    pub sfc: Long,

    /// Vector Base Register (68010+)
    pub vbr: Address,

    /// Cache Address Register (68020+)
    pub caar: Address,

    /// Cache Control Register (68020+)
    pub cacr: RegisterCACR,

    /// Master Stack Pointer (68020+)
    pub msp: Address,

    /// FPU registers
    /// TODO serialization of FPU registers
    #[serde(skip)]
    pub fpu: FpuRegisterFile,

    /// PMMU registers
    pub pmmu: PmmuRegisterFile,
}

impl RegisterFile {
    pub fn new() -> Self {
        Default::default()
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
        out.push_str(diff("SSP/ISP".to_string(), self.isp, other.isp).as_str());
        // PC skipped
        // 68010+
        out.push_str(diff("DFC".to_string(), self.dfc, other.dfc).as_str());
        out.push_str(diff("SFC".to_string(), self.sfc, other.sfc).as_str());
        out.push_str(diff("VBR".to_string(), self.vbr, other.vbr).as_str());
        // 68020+
        out.push_str(diff("CAAR".to_string(), self.caar, other.caar).as_str());
        out.push_str(diff("CACR".to_string(), self.cacr.0, other.cacr.0).as_str());
        out.push_str(diff("MSP".to_string(), self.msp, other.msp).as_str());

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
        out.push_str(&self.fpu.diff_str(&other.fpu));
        out.push_str(&self.pmmu.diff_str(&other.pmmu));
        out
    }

    /// Read an An register
    pub fn read_a<T: CpuSized>(&self, a: usize) -> T {
        T::chop(if a == 7 {
            if self.sr.supervisor() {
                *self.ssp()
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
                let result = *self.ssp();
                *self.ssp_mut() = self.ssp().wrapping_add(adjust);
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
                *self.ssp_mut() = self.ssp().wrapping_sub(adjust);
                *self.ssp()
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
                *self.ssp_mut() = adj_val;
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
            Register::SSP => *self.ssp_mut() = value.expand(),
            Register::PC => panic!("Must be written through CpuM68k::set_pc"),
            Register::SR => self.sr.set_sr(value.expand() as u16),
            Register::DFC => self.dfc = value.expand() & 0b111,
            Register::SFC => self.sfc = value.expand() & 0b111,
            Register::VBR => self.vbr = value.expand(),
            Register::CAAR => self.caar = value.expand(),
            Register::CACR => self.cacr.0 = value.expand() & 0b1111,
            Register::MSP => self.msp = value.expand(),
            Register::ISP => self.isp = value.expand(),
            Register::FPCR => self.fpu.fpcr.0 = value.expand(),
            Register::FPSR => self.fpu.fpsr.0 = value.expand(),
            Register::FPIAR => self.fpu.fpiar = value.expand(),
        }
    }

    /// Read a register, specifying a Register type
    pub fn read<T: CpuSized>(&self, reg: Register) -> T {
        match reg {
            Register::An(r) => self.read_a(r),
            Register::Dn(r) => self.read_d(r),
            Register::USP => T::chop(self.usp),
            Register::SSP => T::chop(*self.ssp()),
            Register::PC => T::chop(self.pc),
            Register::SR => T::chop(self.sr.sr().into()),
            Register::DFC => T::chop(self.dfc & 0b111),
            Register::SFC => T::chop(self.sfc & 0b111),
            Register::VBR => T::chop(self.vbr),
            Register::CAAR => T::chop(self.caar),
            Register::CACR => T::chop(self.cacr.0),
            Register::MSP => T::chop(self.msp),
            Register::ISP => T::chop(self.isp),
            Register::FPCR => T::chop(self.fpu.fpcr.0),
            Register::FPSR => T::chop(self.fpu.fpsr.0),
            Register::FPIAR => T::chop(self.fpu.fpiar),
        }
    }

    /// Reference to active SSP, mutable
    pub fn ssp_mut(&mut self) -> &mut Address {
        if self.sr.m() {
            &mut self.msp
        } else {
            &mut self.isp
        }
    }

    /// Reference to active SSP
    pub fn ssp(&self) -> &Address {
        if self.sr.m() {
            &self.msp
        } else {
            &self.isp
        }
    }
}

impl fmt::Display for RegisterFile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "A: {:X?} D: {:X?} USP: {:08X} SSP: {:08X} PC: {:08X} SR: {:X?}",
            self.a, self.d, self.usp, self.isp, self.pc, self.sr
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
        r.isp = 0;
        r.usp = 0;

        r.write_a(7, 0x11223344_u32);
        assert_eq!(r.usp, 0x11223344);
        assert_eq!(r.isp, 0);
        assert_eq!(r.msp, 0);
    }

    #[test]
    fn write_a7_user_m() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(false);
        r.sr.set_m(true);
        r.isp = 0;
        r.usp = 0;

        r.write_a(7, 0x11223344_u32);
        assert_eq!(r.usp, 0x11223344);
        assert_eq!(r.isp, 0);
        assert_eq!(r.msp, 0);
    }

    #[test]
    fn write_a7_supervisor() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(true);
        r.isp = 0;
        r.usp = 0;

        r.write_a(7, 0x11223344_u32);
        assert_eq!(r.isp, 0x11223344);
        assert_eq!(r.usp, 0);
        assert_eq!(r.msp, 0);
    }

    #[test]
    fn write_a7_supervisor_m() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(true);
        r.sr.set_m(true);
        r.isp = 0;
        r.usp = 0;
        r.msp = 0;

        r.write_a(7, 0x11223344_u32);
        assert_eq!(r.isp, 0);
        assert_eq!(r.usp, 0);
        assert_eq!(r.msp, 0x11223344);
    }

    #[test]
    fn read_a7_user() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(false);
        r.isp = 0;
        r.usp = 0x11223344;

        assert_eq!(r.read_a::<Long>(7), 0x11223344_u32);
    }

    #[test]
    fn read_a7_user_m() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(false);
        r.sr.set_m(true);
        r.isp = 0;
        r.usp = 0x11223344;

        assert_eq!(r.read_a::<Long>(7), 0x11223344_u32);
    }

    #[test]
    fn read_a7_supervisor() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(true);
        r.usp = 0;
        r.isp = 0x11223344;

        assert_eq!(r.read_a::<Long>(7), 0x11223344_u32);
    }

    #[test]
    fn read_a7_supervisor_m() {
        let mut r = RegisterFile::new();
        r.sr.set_supervisor(true);
        r.sr.set_m(true);
        r.usp = 0;
        r.isp = 0;
        r.msp = 0x11223344;

        assert_eq!(r.read_a::<Long>(7), 0x11223344_u32);
    }
}
