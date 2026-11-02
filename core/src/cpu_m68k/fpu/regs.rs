use crate::bus::Address;
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::types::{Byte, Long, Word};
use arpfloat::Float;
use num_traits::Zero;
use proc_bitfield::bitfield;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

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

impl FpuAccruedExceptions {
    pub fn accrue(&mut self, exc: &FpuExceptions) {
        // 2.3.4
        self.set_iop(self.iop() || exc.bsun() || exc.snan() || exc.operr());
        self.set_ovfl(self.ovfl() || exc.ovfl());
        self.set_unfl(self.unfl() || (exc.unfl() && exc.inex2()));
        self.set_dz(self.dz() || exc.dz());
        self.set_inex(self.inex() || exc.inex1() || exc.inex2() || exc.ovfl());
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

/// The busy state frame of the MC68040, with an instruction in the FPU that needs the
/// exception handler (MC68040UM figure 9-10).
///
/// The bytes are the state: the handler reads and changes them, and what's in FRESTORE's
/// frame is what the FPU has to go on with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Busy040(#[serde(with = "BigArray")] pub [u8; Self::FRAME_SIZE]);

impl Busy040 {
    /// Size of the busy state frame in bytes
    pub const FRAME_SIZE: usize = 100;

    const CMDREG3B: usize = 0x34;
    const STAG: usize = 0x3C;
    const CMDREG1B: usize = 0x40;
    const DTAG: usize = 0x44;
    const E_BYTE: usize = 0x48;
    const T_BYTE: usize = 0x49;
    const FPTEMP: usize = 0x4C;
    const ETEMP: usize = 0x58;
    /// Bit in the byte of a tag: bit 15 of the 16-bit internal exponent of the operand
    const E15: u8 = 1 << 4;
    /// Bits of E_BYTE and T_BYTE (from the M68040FPSP)
    const E1: u8 = 1 << 2;
    const E3: u8 = 1 << 1;
    const T: u8 = 1 << 4;

    /// For an unsupported data type: E1 is always set, and T for a move to memory
    /// (MC68040UM table 9-16)
    pub fn unsupported(
        cmdreg1b: Word,
        (etemp, stag): ([Long; 3], u8),
        (fptemp, dtag): ([Long; 3], u8),
        fpiar: Address,
        move_out: bool,
    ) -> Self {
        // FSQRT has $05 in the frame, for $04 in the instruction (MC68040UM 9.8)
        let cmdreg1b = if cmdreg1b & 0x7F == 0x04 {
            cmdreg1b | 0x01
        } else {
            cmdreg1b
        };
        let mut frame = [0; Self::FRAME_SIZE];
        frame[0..2].copy_from_slice(&0x4160u16.to_be_bytes());
        frame[0x28..0x2C].copy_from_slice(&fpiar.to_be_bytes());
        frame[Self::STAG] = stag << 5;
        frame[Self::CMDREG1B..Self::CMDREG1B + 2].copy_from_slice(&cmdreg1b.to_be_bytes());
        frame[Self::DTAG] = dtag << 5;
        frame[Self::E_BYTE] = Self::E1;
        frame[Self::T_BYTE] = if move_out { Self::T } else { 0 };
        for (i, long) in fptemp.into_iter().enumerate() {
            frame[Self::FPTEMP + 4 * i..Self::FPTEMP + 4 * i + 4]
                .copy_from_slice(&long.to_be_bytes());
        }
        for (i, long) in etemp.into_iter().enumerate() {
            frame[Self::ETEMP + 4 * i..Self::ETEMP + 4 * i + 4]
                .copy_from_slice(&long.to_be_bytes());
        }
        Self(frame)
    }

    /// An exception is still to report: E1 or E3
    pub fn has_exception(&self) -> bool {
        self.0[Self::E_BYTE] & (Self::E1 | Self::E3) != 0
    }

    /// The command word of the instruction. FSQRT has $05 in the frame, for $04 in the
    /// instruction (MC68040UM 9.8).
    pub fn command(&self) -> Word {
        let word = Word::from_be_bytes([self.0[Self::CMDREG1B], self.0[Self::CMDREG1B + 1]]);
        if word & 0x7F == 0x05 {
            (word & !0x7F) | 0x04
        } else {
            word
        }
    }

    /// The encoded command word for an exception of the write-back stage (MC68040UM
    /// figure 9-11)
    pub fn command3(&self) -> Word {
        Word::from_be_bytes([self.0[Self::CMDREG3B], self.0[Self::CMDREG3B + 1]])
    }

    fn operand(&self, at: usize, tag: usize) -> BitsExtReal {
        let long =
            |i: usize| Long::from_be_bytes(self.0[at + 4 * i..at + 4 * i + 4].try_into().unwrap());
        let bits = BitsExtReal::default()
            .with_high(long(0) & 0xFFFF_0000)
            .with_mid(long(1))
            .with_low(long(2));
        // A denormalized number the handler normalized has a negative exponent, with bit
        // 15 of the internal exponent set (the M68040FPSP, res_func). It's the same
        // number as a denormalized one of the extended format, so it goes back to that.
        if self.0[tag] & Self::E15 != 0 && bits.i() && bits.e() >= 0x4000 {
            let shift = 0x8000 - bits.e() as u32;
            let man = if shift > 63 {
                0
            } else {
                bits.raw_mantissa() >> shift
            };
            return bits.with_e(0).with_raw_mantissa(man);
        }
        bits
    }

    /// The source operand, as the handler left it
    pub fn source(&self) -> BitsExtReal {
        self.operand(Self::ETEMP, Self::STAG)
    }

    /// The destination operand, as the handler left it
    pub fn dest(&self) -> BitsExtReal {
        self.operand(Self::FPTEMP, Self::DTAG)
    }
}

/// Internal state of an MC68040 FPU after an unimplemented floating-point
/// instruction exception, which FSAVE saves as the unimplemented instruction
/// state frame (MC68040UM 9.6.1, 9.8)
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unimp040 {
    /// Command word of the instruction (CMDREG1B)
    pub cmdreg1b: Word,
    /// Source operand in extended precision, as in memory (ETEMP)
    pub etemp: [Long; 3],
    /// Data type of the source operand (STAG)
    pub stag: u8,
    /// Destination operand in extended precision, as in memory (FPTEMP)
    pub fptemp: [Long; 3],
    /// Data type of the destination operand (DTAG)
    pub dtag: u8,
    /// The source operand is a packed decimal real, kept as it is: bits
    /// 63-0 in ETEMP, bits 95-64 in FPTEMP (E1)
    pub packed: bool,
}

impl Unimp040 {
    /// Size of the unimplemented instruction state frame in bytes
    pub const FRAME_SIZE: usize = 52;

    /// Format long word of the unimplemented instruction state frame
    pub const FRAME_FORMAT: Long = 0x4130_0000;

    /// An operand in extended precision as in memory, and its data type
    /// tag (STAG or DTAG, MC68040UM 9.8)
    pub fn operand(bits: &BitsExtReal) -> ([Long; 3], u8) {
        let tag = if bits.is_zero() {
            0b001
        } else if bits.is_inf() {
            0b010
        } else if bits.is_nan() {
            0b011
        } else if !bits.i() {
            // Denormalized or unnormalized
            0b100
        } else {
            0b000
        };
        ([bits.high(), bits.mid(), bits.low()], tag)
    }

    /// The unimplemented instruction state frame (MC68040UM figure 9-10 (d),
    /// table 9-16)
    pub fn to_frame(&self) -> [u8; Self::FRAME_SIZE] {
        let mut longs = [0; Self::FRAME_SIZE / 4];
        longs[0] = Self::FRAME_FORMAT;
        longs[3] = Long::from(self.stag) << 29;
        longs[4] = Long::from(self.cmdreg1b) << 16;
        longs[5] = Long::from(self.dtag) << 29;
        // E1 for a packed decimal real source, T clear. This differs from
        // table 9-16 of the MC68040UM, which has E1 always set for an
        // unimplemented instruction, but follows the definition of E1 in
        // 9.8: "for the unimplemented instruction state frame, the source
        // operand's unsupported data type is packed if the E1 bit is set".
        // The M68040FPSP does the same: with E1 always set, running it in
        // Snow took every source operand for a packed decimal real and gave
        // wrong results.
        longs[6] = Long::from(self.packed) << 26;
        longs[7..10].copy_from_slice(&self.fptemp);
        longs[10..13].copy_from_slice(&self.etemp);

        let mut frame = [0; Self::FRAME_SIZE];
        for (bytes, long) in frame.chunks_exact_mut(4).zip(longs) {
            bytes.copy_from_slice(&long.to_be_bytes());
        }
        frame
    }

    /// The state an unimplemented instruction state frame holds
    pub fn from_frame(frame: &[u8; Self::FRAME_SIZE]) -> Self {
        let long = |i: usize| Long::from_be_bytes(frame[4 * i..4 * i + 4].try_into().unwrap());
        Self {
            cmdreg1b: (long(4) >> 16) as Word,
            etemp: [long(10), long(11), long(12)],
            stag: (long(3) >> 29) as u8,
            fptemp: [long(7), long(8), long(9)],
            dtag: (long(5) >> 29) as u8,
            packed: long(6) & (1 << 26) != 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpuRegisterFile {
    /// The data registers, in extended precision, with all their bits
    pub fp: [BitsExtReal; 8],
    pub fpcr: RegisterFPCR,
    pub fpsr: RegisterFPSR,
    pub fpiar: Address,
    /// Not in the null state of a reset: FSAVE saves an idle frame
    #[serde(default)]
    pub used: bool,
    /// MC68040: the instruction waiting for the exception handler, until FSAVE
    #[serde(default)]
    pub busy040: Option<Busy040>,
    /// MC68040: the vector of an exception to report at the next floating-point
    /// instruction but FSAVE (MC68040UM 9.6.2)
    #[serde(default)]
    pub pending040: Option<Address>,
    /// A floating-point exception is pending, to report at the next floating-point
    /// instruction, with the type from FPSR and FPCR: EXC PEND (MC68881UM 6.4.2.2)
    #[serde(default)]
    pub exc_pending: bool,
    /// The operand for the exception handler, in FSAVE's idle frame (MC68881UM 6.1)
    #[serde(default)]
    pub exc_operand: BitsExtReal,
    /// The command or conditional predicate of the last instruction, in FSAVE's idle frame
    #[serde(default)]
    pub command: Word,
    /// MC68040: state after an unimplemented floating-point instruction
    /// exception, until FSAVE
    pub unimp040: Option<Unimp040>,
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
        let diff_fp = |name, s: &BitsExtReal, o: &BitsExtReal| {
            if s != o {
                format!(
                    "{}: {:0.4} -> {:0.4} ",
                    name,
                    Float::from(*s).as_f64(),
                    Float::from(*o).as_f64()
                )
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
            fp: [BitsExtReal::nan(false); 8],
            fpcr: RegisterFPCR(0),
            fpsr: RegisterFPSR(0),
            fpiar: Address::zero(),
            used: false,
            busy040: None,
            pending040: None,
            exc_pending: false,
            exc_operand: BitsExtReal::default(),
            command: 0,
            unimp040: None,
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
