use std::fmt;

use num_derive::ToPrimitive;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, IntoEnumIterator};

/// Bit positions of the flags in the P register.
#[derive(EnumIter, ToPrimitive, Debug, Copy, Clone, Display)]
pub enum Flag {
    /// Negative
    N = 7,
    /// Overflow
    V = 6,
    /// Accumulator/memory width (native mode)
    M = 5,
    /// Index register width (native mode)
    X = 4,
    /// Decimal mode
    D = 3,
    /// Interrupt disable
    I = 2,
    /// Zero
    Z = 1,
    /// Carry
    C = 0,
}

impl Flag {
    /// Break flag (emulation mode)
    pub const B: Self = Self::X;
}

/// Bit-width of a register (see Register::width())
#[derive(Debug, Eq, PartialEq)]
pub enum RegisterWidth {
    EightBit,
    SixteenBit,
}

/// Enumeration of registers
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Register {
    /// Accumulator (lower 8-bit)
    A,
    /// Accumulator (upper 8-bit)
    B,
    /// Accumulator (full 16-bit)
    C,
    /// Data Bank Register
    DBR,
    /// Direct register (full 16-bit)
    D,
    /// Direct register (lower 8-bit)
    DL,
    /// Direct register (upper 8-bit)
    DH,
    /// Program Bank register (8-bit)
    K,
    /// Program Counter (full 16-bit)
    PC,
    /// Program Counter (lower 8-bit)
    PCL,
    /// Program Counter (upper 8-bit)
    PCH,
    /// Processor Status register
    P,
    /// Stack pointer (full 16-bit)
    S,
    /// Stack pointer (lower 8-bit)
    SL,
    /// Stack pointer (upper 8-bit)
    SH,
    /// X-index (full 16-bit)
    X,
    /// X-index (lower 8-bit)
    XL,
    /// X-index (upper 8-bit)
    XH,
    /// Y-index (full 16-bit)
    Y,
    /// Y-index (lower 8-bit)
    YL,
    /// Y-index (upper 8-bit)
    YH,
}

impl Register {
    /// Width of the register.
    pub const fn width(&self) -> RegisterWidth {
        match self {
            Register::A
            | Register::B
            | Register::DBR
            | Register::DL
            | Register::DH
            | Register::K
            | Register::P
            | Register::PCL
            | Register::PCH
            | Register::SL
            | Register::SH
            | Register::XL
            | Register::XH
            | Register::YL
            | Register::YH => RegisterWidth::EightBit,

            Register::C | Register::D | Register::PC | Register::S | Register::X | Register::Y => {
                RegisterWidth::SixteenBit
            }
        }
    }
}

/// Complete CPU register file
#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct RegisterFile {
    pub c: u16,
    pub dbr: u8,
    pub d: u16,
    pub k: u8,
    pub pc: u16,
    pub p: u8,
    pub s: u16,
    pub x: u16,
    pub y: u16,
    pub emulation: bool,
}

impl RegisterFile {
    pub fn new() -> Self {
        Self {
            c: 0,
            dbr: 0,
            d: 0,
            k: 0,
            pc: 0,
            p: 0,
            s: 0,
            x: 0,
            y: 0,
            emulation: false,
        }
    }

    /// Get the complete (24-bit) program counter
    pub fn get_full_pc(&self) -> u32 {
        (self.k as u32) << 16 | self.pc as u32
    }

    /// Write a value to a register.
    pub fn write(&mut self, reg: Register, val: u16) {
        let reg8 = || {
            assert!(val <= u8::MAX.into());
            val as u8
        };
        let reg16 = || val;
        let reg16_high = |old: u16| {
            // Replace MSB
            assert!(val <= u8::MAX.into());
            old & 0xFF | (val << 8)
        };
        let reg16_low = |old: u16| {
            // Replace LSB
            assert!(val <= u8::MAX.into());
            val & 0xFF | (old & 0xFF00)
        };

        match reg {
            // Pure 8-bit registers
            Register::DBR => self.dbr = reg8(),
            Register::K => self.k = reg8(),
            Register::P => {
                self.p = reg8();
                self.update_flags();
            }

            // Lower/higher 8-bit of 16-bit registers
            Register::A => self.c = reg16_low(self.c),
            Register::B => self.c = reg16_high(self.c),
            Register::DL => self.d = reg16_low(self.d),
            Register::DH => self.d = reg16_high(self.d),
            Register::PCL => self.pc = reg16_low(self.pc),
            Register::PCH => self.pc = reg16_high(self.pc),
            Register::SL => self.s = reg16_low(self.s),
            Register::SH => self.s = reg16_high(self.s),
            Register::XL => self.x = reg16_low(self.x),
            Register::XH => self.x = reg16_high(self.x),
            Register::YL => self.y = reg16_low(self.y),
            Register::YH => self.y = reg16_high(self.y),

            // 16-bit registers
            Register::C => self.c = reg16(),
            Register::D => self.d = reg16(),
            Register::PC => self.pc = reg16(),
            Register::S => self.s = reg16(),
            Register::X => self.x = reg16(),
            Register::Y => self.y = reg16(),
        }
    }

    /// Read an 8-bit or 16-bit register.
    pub fn read(&self, reg: Register) -> u16 {
        let reg16_low = |r| r & 0xFF;
        let reg16_high = |r| r >> 8;

        match reg {
            // Pure 8-bit registers
            Register::DBR => self.dbr.into(),
            Register::K => self.k.into(),
            Register::P => self.p.into(),

            // Lower/higher 8-bit of 16-bit registers
            Register::A => reg16_low(self.c),
            Register::B => reg16_high(self.c),
            Register::DL => reg16_low(self.d),
            Register::DH => reg16_high(self.d),
            Register::PCL => reg16_low(self.pc),
            Register::PCH => reg16_high(self.pc),
            Register::SL => reg16_low(self.s),
            Register::SH => reg16_high(self.s),
            Register::XL => reg16_low(self.x),
            Register::XH => reg16_high(self.x),
            Register::YL => reg16_low(self.y),
            Register::YH => reg16_high(self.y),

            // 16-bit registers
            Register::C => self.c,
            Register::D => self.d,
            Register::PC => self.pc,
            Register::S => self.s,
            Register::X => self.x,
            Register::Y => self.y,
        }
    }

    /// Reads an 8-bit register
    pub fn read8(&self, reg: Register) -> u8 {
        assert_eq!(reg.width(), RegisterWidth::EightBit);
        self.read(reg) as u8
    }

    /// Reads an 16-bit register
    pub fn read16(&self, reg: Register) -> u16 {
        assert_eq!(reg.width(), RegisterWidth::SixteenBit);
        self.read(reg)
    }

    /// Read register and post-increment.
    pub fn read_inc(&mut self, reg: Register) -> u16 {
        match reg.width() {
            RegisterWidth::EightBit => self.read8_inc(reg) as u16,
            RegisterWidth::SixteenBit => self.read16_inc(reg),
        }
    }

    /// Read register and post-decrement.
    pub fn read_dec(&mut self, reg: Register) -> u16 {
        match reg.width() {
            RegisterWidth::EightBit => self.read8_dec(reg).into(),
            RegisterWidth::SixteenBit => self.read16_dec(reg),
        }
    }

    /// Read 8-bit register and post-increment.
    pub fn read8_inc(&mut self, reg: Register) -> u8 {
        let val = self.read8(reg);
        self.write(reg, val.wrapping_add(1) as u16);
        val
    }

    /// Read 16-bit register and post-increment.
    pub fn read16_inc(&mut self, reg: Register) -> u16 {
        let val = self.read16(reg);
        self.write(reg, val.wrapping_add(1));
        val
    }

    /// Read 8-bit register and post-decrement.
    pub fn read8_dec(&mut self, reg: Register) -> u8 {
        let val = self.read8(reg);
        self.write(reg, val.wrapping_sub(1) as u16);
        val
    }

    /// Read 16-bit register and post-decrement.
    pub fn read16_dec(&mut self, reg: Register) -> u16 {
        let val = self.read16(reg);
        self.write(reg, val.wrapping_sub(1));
        val
    }

    /// Clear and write the flags in P.
    pub fn write_flags(&mut self, flag_val: &[(Flag, bool)]) {
        let mut p: u8 = self.p;
        for &(b, on) in flag_val {
            let bit = 1u8 << b.to_u8().unwrap();
            p &= !bit;
            if on {
                p |= bit;
            }
        }
        self.p = p;
        self.update_flags();
    }

    /// Test a flag in P.
    pub fn test_flag(&self, f: Flag) -> bool {
        self.p & (1u8 << f.to_u8().unwrap()) != 0
    }

    /// Take actions resulting of a flags update.
    fn update_flags(&mut self) {
        if self.test_flag(Flag::X) {
            self.x &= 0xFF;
            self.y &= 0xFF;
        }
    }
}

impl fmt::Display for RegisterFile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let flags: String = Flag::iter()
            .map(|f| (f, f.to_string().chars().nth(0).unwrap()))
            .map(|(f, fc)| {
                if self.p & 1 << f.to_u8().unwrap() != 0 {
                    fc.to_uppercase().next().unwrap()
                } else {
                    fc.to_lowercase().next().unwrap()
                }
            })
            .collect::<String>();

        write!(
            f,
            "C:{:04X} DBR:{:02X} D:{:04X} K|PC:{:02X}|{:04X} S:{:04X} X:{:04X} Y:{:04X} P:{:02X} ({}) E:{}",
            self.c, self.dbr, self.d, self.k, self.pc, self.s, self.x, self.y, self.p, flags, self.emulation
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_8bit() {
        let mut r = RegisterFile::new();
        r.write(Register::P, 0x12);
        assert_eq!(r.p, 0x12);

        let mut r = RegisterFile::new();
        r.write(Register::K, 0x12);
        assert_eq!(r.k, 0x12);

        let mut r = RegisterFile::new();
        r.write(Register::DBR, 0x12);
        assert_eq!(r.dbr, 0x12);
    }

    #[test]
    #[should_panic]
    fn write_8bit_error() {
        let mut r = RegisterFile::new();
        r.write(Register::K, 0x1FF);
    }

    #[test]
    fn write_16bit() {
        let mut r = RegisterFile::new();
        r.write(Register::C, 0x1234);
        assert_eq!(r.c, 0x1234);

        let mut r = RegisterFile::new();
        r.write(Register::D, 0x1234);
        assert_eq!(r.d, 0x1234);

        let mut r = RegisterFile::new();
        r.write(Register::X, 0x1234);
        assert_eq!(r.x, 0x1234);

        let mut r = RegisterFile::new();
        r.write(Register::Y, 0x1234);
        assert_eq!(r.y, 0x1234);

        let mut r = RegisterFile::new();
        r.write(Register::PC, 0x1234);
        assert_eq!(r.pc, 0x1234);

        let mut r = RegisterFile::new();
        r.write(Register::S, 0x1234);
        assert_eq!(r.s, 0x1234);
    }

    #[test]
    fn write_16bit_h() {
        let mut r = RegisterFile::new();
        r.d = 0xEEFF;
        r.write(Register::DH, 0x12);
        assert_eq!(r.d, 0x12FF);

        let mut r = RegisterFile::new();
        r.x = 0xEEFF;
        r.write(Register::XH, 0x12);
        assert_eq!(r.x, 0x12FF);

        let mut r = RegisterFile::new();
        r.y = 0xEEFF;
        r.write(Register::YH, 0x12);
        assert_eq!(r.y, 0x12FF);

        let mut r = RegisterFile::new();
        r.pc = 0xEEFF;
        r.write(Register::PCH, 0x12);
        assert_eq!(r.pc, 0x12FF);

        let mut r = RegisterFile::new();
        r.s = 0xEEFF;
        r.write(Register::SH, 0x12);
        assert_eq!(r.s, 0x12FF);

        let mut r = RegisterFile::new();
        r.c = 0xEEFF;
        r.write(Register::B, 0x12);
        assert_eq!(r.c, 0x12FF);
    }

    #[test]
    fn write_16bit_l() {
        let mut r = RegisterFile::new();
        r.d = 0xEEFF;
        r.write(Register::DL, 0x12);
        assert_eq!(r.d, 0xEE12);

        let mut r = RegisterFile::new();
        r.x = 0xEEFF;
        r.write(Register::XL, 0x12);
        assert_eq!(r.x, 0xEE12);

        let mut r = RegisterFile::new();
        r.y = 0xEEFF;
        r.write(Register::YL, 0x12);
        assert_eq!(r.y, 0xEE12);

        let mut r = RegisterFile::new();
        r.pc = 0xEEFF;
        r.write(Register::PCL, 0x12);
        assert_eq!(r.pc, 0xEE12);

        let mut r = RegisterFile::new();
        r.s = 0xEEFF;
        r.write(Register::SL, 0x12);
        assert_eq!(r.s, 0xEE12);

        let mut r = RegisterFile::new();
        r.c = 0xEEFF;
        r.write(Register::A, 0x12);
        assert_eq!(r.c, 0xEE12);
    }

    #[test]
    fn read8() {
        let mut r = RegisterFile::new();
        r.k = 0x12;
        assert_eq!(r.read8(Register::K), 0x12);

        let mut r = RegisterFile::new();
        r.dbr = 0x12;
        assert_eq!(r.read8(Register::DBR), 0x12);

        let mut r = RegisterFile::new();
        r.p = 0x12;
        assert_eq!(r.read8(Register::P), 0x12);
    }

    #[test]
    #[should_panic]
    fn read8_error() {
        let r = RegisterFile::new();
        r.read8(Register::C);
    }

    #[test]
    fn read16() {
        let mut r = RegisterFile::new();
        r.c = 0x1234;
        assert_eq!(r.read16(Register::C), 0x1234);

        let mut r = RegisterFile::new();
        r.s = 0x1234;
        assert_eq!(r.read16(Register::S), 0x1234);

        let mut r = RegisterFile::new();
        r.d = 0x1234;
        assert_eq!(r.read16(Register::D), 0x1234);

        let mut r = RegisterFile::new();
        r.x = 0x1234;
        assert_eq!(r.read16(Register::X), 0x1234);

        let mut r = RegisterFile::new();
        r.y = 0x1234;
        assert_eq!(r.read16(Register::Y), 0x1234);

        let mut r = RegisterFile::new();
        r.pc = 0x1234;
        assert_eq!(r.read16(Register::PC), 0x1234);
    }

    #[test]
    #[should_panic]
    fn read16_error() {
        let r = RegisterFile::new();
        r.read16(Register::K);
    }

    #[test]
    fn write_flags_none() {
        let mut r = RegisterFile::new();
        r.p = 0xF0;
        r.write_flags(&[]);
        assert_eq!(r.p, 0xF0);
    }

    #[test]
    fn write_flags_one_true() {
        let mut r = RegisterFile::new();
        r.write_flags(&[(Flag::N, true)]);
        assert_eq!(r.p, 1u8 << Flag::N.to_u8().unwrap());
    }

    #[test]
    fn write_flags_one_false() {
        let mut r = RegisterFile::new();
        r.write_flags(&[(Flag::N, false)]);
        assert_eq!(r.p, 0);
    }

    #[test]
    fn write_flags_multiple_true() {
        let mut r = RegisterFile::new();
        r.write_flags(&[(Flag::N, true), (Flag::C, true)]);
        assert_eq!(
            r.p,
            1u8 << Flag::N.to_u8().unwrap() | 1u8 << Flag::C.to_u8().unwrap()
        );
    }

    #[test]
    fn write_flags_multiple_mixed() {
        let mut r = RegisterFile::new();
        r.write_flags(&[
            (Flag::Z, false),
            (Flag::C, false),
            (Flag::V, true),
            (Flag::N, true),
        ]);
        assert_eq!(
            r.p,
            1u8 << Flag::V.to_u8().unwrap() | 1u8 << Flag::N.to_u8().unwrap()
        );
    }

    #[test]
    fn write_flags_unwritten_untouched() {
        let mut r = RegisterFile::new();
        r.write_flags(&[(Flag::Z, true)]);
        r.write_flags(&[(Flag::N, true)]);
        assert!(r.test_flag(Flag::Z));
    }

    #[test]
    fn read8_inc() {
        let mut r = RegisterFile::new();
        r.dbr = 12;
        assert_eq!(r.read8_inc(Register::DBR), 12);
        assert_eq!(r.dbr, 13);
    }

    #[test]
    fn read8_inc_overflow() {
        let mut r = RegisterFile::new();
        r.k = 0xFF;
        assert_eq!(r.read8_inc(Register::K), 0xFF);
        assert_eq!(r.k, 0);
    }

    #[test]
    fn read8_dec() {
        let mut r = RegisterFile::new();
        r.k = 12;
        assert_eq!(r.read8_dec(Register::K), 12);
        assert_eq!(r.k, 11);
    }

    #[test]
    fn read8_dec_overflow() {
        let mut r = RegisterFile::new();
        r.k = 0;
        assert_eq!(r.read8_dec(Register::K), 0);
        assert_eq!(r.k, 0xFF);
    }

    #[test]
    fn read16_inc() {
        let mut r = RegisterFile::new();
        r.c = 0x1234;
        assert_eq!(r.read16_inc(Register::C), 0x1234);
        assert_eq!(r.c, 0x1235);
    }

    #[test]
    fn read16_inc_overflow() {
        let mut r = RegisterFile::new();
        r.c = 0xFFFF;
        assert_eq!(r.read16_inc(Register::C), 0xFFFF);
        assert_eq!(r.c, 0);
    }

    #[test]
    fn read16_dec() {
        let mut r = RegisterFile::new();
        r.c = 0x1234;
        assert_eq!(r.read16_dec(Register::C), 0x1234);
        assert_eq!(r.c, 0x1233);
    }

    #[test]
    fn read16_dec_overflow() {
        let mut r = RegisterFile::new();
        r.c = 0;
        assert_eq!(r.read16_dec(Register::C), 0);
        assert_eq!(r.c, 0xFFFF);
    }

    #[test]
    fn read_inc() {
        let mut r = RegisterFile::new();
        r.k = 12;
        assert_eq!(r.read_inc(Register::K), 12);
        assert_eq!(r.k, 13);

        let mut r = RegisterFile::new();
        r.c = 0x1234;
        assert_eq!(r.read_inc(Register::C), 0x1234);
        assert_eq!(r.c, 0x1235);
    }

    #[test]
    fn read_dec() {
        let mut r = RegisterFile::new();
        r.k = 12;
        assert_eq!(r.read_dec(Register::K), 12);
        assert_eq!(r.k, 11);

        let mut r = RegisterFile::new();
        r.c = 0x1234;
        assert_eq!(r.read_dec(Register::C), 0x1234);
        assert_eq!(r.c, 0x1233);
    }
}
