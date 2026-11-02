//! 6888x behaviour from the MC68881/MC68882 User's Manual

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::storage::BitsExtReal;
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};
use arpfloat::Float;

type TestCpu = CpuM68030Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;

fn testcpu(code: &[Word]) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    for (i, w) in code.iter().enumerate() {
        let [h, l] = w.to_be_bytes();
        cpu.bus.mem.insert(CODE + 2 * i as Address, h);
        cpu.bus.mem.insert(CODE + 2 * i as Address + 1, l);
    }
    // Privilege violation
    cpu.bus.mem.insert(0x22, 0x04);
    cpu.regs.isp = 0x8000;
    cpu.regs.usp = 0x7000;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.a[0] = 0x3000;
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    let b = |a: Address| Long::from(*cpu.bus.mem.get(&a).unwrap_or(&0));
    (b(addr) << 24) | (b(addr + 1) << 16) | (b(addr + 2) << 8) | b(addr + 3)
}

fn fp(v: f64) -> BitsExtReal {
    BitsExtReal::from(&Float::from_f64(v).cast(SEMANTICS_EXTENDED))
}

/// FOP FPs,FPd
fn fop(src: Word, dst: Word, opmode: Word) -> [Word; 2] {
    [0xF200, (src << 10) | (dst << 7) | opmode]
}

#[test]
fn fmovem_control_to_memory_ascends() {
    // FMOVEM.L FPCR/FPSR,(A0)
    let mut cpu = testcpu(&[0xF210, 0xB800]);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.regs.fpu.fpsr.0 = 0x0800_0000;
    cpu.step().unwrap();
    assert_eq!(read_long(&cpu, 0x3000), 0x10);
    assert_eq!(read_long(&cpu, 0x3004), 0x0800_0000);
    assert_eq!(cpu.regs.a[0], 0x3000);
}

#[test]
fn fmovem_control_predecrement_has_the_same_layout() {
    // FMOVEM.L FPCR/FPSR/FPIAR,-(A0)
    let mut cpu = testcpu(&[0xF220, 0xBC00]);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.regs.fpu.fpsr.0 = 0x0800_0000;
    cpu.regs.fpu.fpiar = 0x1234;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.a[0], 0x3000 - 12);
    assert_eq!(read_long(&cpu, 0x3000 - 12), 0x10);
    assert_eq!(read_long(&cpu, 0x3000 - 8), 0x0800_0000);
    assert_eq!(read_long(&cpu, 0x3000 - 4), 0x1234);
}

#[test]
fn fmovem_control_save_and_restore() {
    // FMOVEM.L FPCR/FPSR,-(A0) ; FMOVEM.L (A0)+,FPCR/FPSR
    let mut cpu = testcpu(&[0xF220, 0xB800, 0xF218, 0x9800]);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.regs.fpu.fpsr.0 = 0x0800_0000;
    cpu.step().unwrap();
    cpu.regs.fpu.fpcr.0 = 0;
    cpu.regs.fpu.fpsr.0 = 0;
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpcr.0, 0x10);
    assert_eq!(cpu.regs.fpu.fpsr.0, 0x0800_0000);
    assert_eq!(cpu.regs.a[0], 0x3000);
}

#[test]
fn fmove_to_long_rounds_with_fpcr() {
    for (v, fpcr, want) in [
        (3.5, 0, 4),
        (2.5, 0, 2),
        (-2.5, 0, -2),
        (1.75, 0, 2),
        (3.5, 0x10, 3),
        (-3.5, 0x20, -4),
        (3.2, 0x30, 4),
    ] {
        let mut cpu = testcpu(&[0xF200, 0x6000]); // FMOVE.L FP0,D0
        cpu.regs.fpu.fpcr.0 = fpcr;
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.d[0] as i32, want, "{v} {fpcr:02X}");
        assert!(cpu.regs.fpu.fpsr.exs().inex2());
        assert!(!cpu.regs.fpu.fpsr.exs().operr());
    }
}

#[test]
fn fmove_to_long_exact_is_not_inexact() {
    let mut cpu = testcpu(&[0xF200, 0x6000]);
    cpu.regs.fpu.fp[0] = fp(-5.0);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.d[0] as i32, -5);
    assert!(!cpu.regs.fpu.fpsr.exs().inex2());
}

#[test]
fn fmove_to_integer_overflow_is_operr() {
    for (op, v, want) in [
        (0x6000, 1e12, 0x7FFF_FFFF),
        (0x6000, -1e12, 0x8000_0000),
        (0x7000, 1e6, 0x7FFF),
        (0x7800, -1e6, 0x80),
    ] {
        let mut cpu = testcpu(&[0xF200, op]);
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.regs.d[0] = 0;
        cpu.step().unwrap();
        assert_eq!(cpu.regs.d[0], want, "{op:04X} {v}");
        assert!(cpu.regs.fpu.fpsr.exs().operr());
        assert!(!cpu.regs.fpu.fpsr.exs().inex2());
    }
}

#[test]
fn fint_rounds_ties_to_even() {
    for (v, fpcr, want) in [
        (2.5, 0, 2.0),
        (3.5, 0, 4.0),
        (-2.5, 0, -2.0),
        (0.5, 0, 0.0),
        (2.6, 0, 3.0),
        (2.9, 0x10, 2.0),
        (2.1, 0x30, 3.0),
        (-2.1, 0x20, -3.0),
    ] {
        let mut cpu = testcpu(&fop(0, 1, 0x01));
        cpu.regs.fpu.fpcr.0 = fpcr;
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.fpu.fp[1].as_f64(), want, "{v} {fpcr:02X}");
    }
}

#[test]
fn rounding_precision_applies_to_the_result_only() {
    let mut cpu = testcpu(&fop(0, 1, 0x22));
    cpu.regs.fpu.fpcr.0 = 0x40; // single
    cpu.regs.fpu.fp[0] = fp(1.0 + 2f64.powi(-24));
    cpu.regs.fpu.fp[1] = fp(2f64.powi(-24));
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[1].as_f64(), 1.0 + 2f64.powi(-23));
}

#[test]
fn fcmp_and_ftst_keep_the_registers() {
    let v = 1.0 + 2f64.powi(-40);
    for opmode in [0x38, 0x3A] {
        let mut cpu = testcpu(&fop(0, 1, opmode));
        cpu.regs.fpu.fpcr.0 = 0x40;
        cpu.regs.fpu.fp[0] = fp(1.0);
        cpu.regs.fpu.fp[1] = fp(v);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.fpu.fp[1].as_f64(), v);
    }
}

#[test]
fn fcmp_special_values() {
    let inf = BitsExtReal::inf;
    let zero = BitsExtReal::zero;
    // src, dest, N, Z, NAN
    for (i, (src, dest, n, z, nan)) in [
        (inf(false), inf(false), false, true, false),
        (inf(true), inf(true), true, true, false),
        (inf(false), inf(true), true, false, false),
        (inf(true), inf(false), false, false, false),
        (zero(false), zero(true), true, true, false),
        (zero(true), zero(false), false, true, false),
        (fp(1.0), fp(2.0), false, false, false),
        (fp(2.0), fp(1.0), true, false, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut cpu = testcpu(&fop(0, 1, 0x38));
        cpu.regs.fpu.fp[0] = src;
        cpu.regs.fpu.fp[1] = dest;
        cpu.step().unwrap();
        let fpsr = cpu.regs.fpu.fpsr;
        assert_eq!(
            (fpsr.fpcc_n(), fpsr.fpcc_z(), fpsr.fpcc_nan()),
            (n, z, nan),
            "{i}"
        );
    }
}

#[test]
fn undefined_precision_does_not_stop() {
    let mut cpu = testcpu(&fop(0, 1, 0x22));
    cpu.regs.fpu.fpcr.0 = 0xC0;
    cpu.regs.fpu.fp[0] = fp(1.0);
    cpu.regs.fpu.fp[1] = fp(1.0);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fp[1].as_f64(), 2.0);
}

#[test]
fn exception_status_of_arithmetic() {
    // src, dest, opmode, DZ, OPERR, OVFL, INEX2
    // About 1e3000, its square overflows
    let big = (0..9).fold(Float::from_f64(1e300).cast(SEMANTICS_EXTENDED), |v, _| {
        v * Float::from_f64(1e300).cast(SEMANTICS_EXTENDED)
    });
    let big = BitsExtReal::from(&big);
    for (i, (src, dest, opmode, dz, operr, ovfl, inex2)) in [
        (fp(0.0), fp(1.0), 0x20, true, false, false, false),
        (fp(0.0), fp(0.0), 0x20, false, true, false, false),
        (fp(3.0), fp(1.0), 0x20, false, false, false, true),
        (fp(2.0), fp(1.0), 0x20, false, false, false, false),
        (fp(-1.0), fp(0.0), 0x04, false, true, false, false),
        (fp(4.0), fp(0.0), 0x04, false, false, false, false),
        (fp(2.0), fp(0.0), 0x04, false, false, false, true),
        (fp(0.5), fp(0.25), 0x22, false, false, false, false),
        (fp(1.0), fp(2f64.powi(-70)), 0x22, false, false, false, true),
        (big, big, 0x23, false, false, true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let mut cpu = testcpu(&fop(0, 1, opmode));
        cpu.regs.fpu.fp[0] = src;
        cpu.regs.fpu.fp[1] = dest;
        cpu.step().unwrap();
        let e = cpu.regs.fpu.fpsr.exs();
        assert_eq!(
            (e.dz(), e.operr(), e.ovfl(), e.inex2()),
            (dz, operr, ovfl, inex2),
            "{i}"
        );
        let a = cpu.regs.fpu.fpsr.aexc();
        assert_eq!(a.dz(), dz, "{i}");
        assert_eq!(a.iop(), operr, "{i}");
    }
}

#[test]
fn accrued_exceptions_stay() {
    // FDIV FP0,FP1 by zero, then FADD FP2,FP3 exact
    let mut cpu = testcpu(&[0xF200, 0x00A0, 0xF200, 0x09A2]);
    cpu.regs.fpu.fp[0] = fp(0.0);
    cpu.regs.fpu.fp[1] = fp(1.0);
    cpu.regs.fpu.fp[2] = fp(1.0);
    cpu.regs.fpu.fp[3] = fp(1.0);
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert!(!cpu.regs.fpu.fpsr.exs().dz());
    assert!(cpu.regs.fpu.fpsr.aexc().dz());
}

#[test]
fn fsave_frestore_are_privileged() {
    for op in [0xF310, 0xF350] {
        let mut cpu = testcpu(&[op]);
        cpu.regs.sr.set_supervisor(false);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.pc, 0x400, "{op:04X}");
        assert_eq!(read_long(&cpu, 0x8000 - 6), CODE, "{op:04X}");
    }
}

#[test]
fn fsave_gives_a_null_frame_until_the_fpu_is_used() {
    // FSAVE -(A0) ; FNOP ; FSAVE -(A0)
    let mut cpu = testcpu(&[0xF320, 0xF280, 0x0000, 0xF320]);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.a[0], 0x3000 - 4);
    assert_eq!(read_long(&cpu, 0x3000 - 4), 0);
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.a[0], 0x3000 - 4 - 60);
    assert_eq!(read_long(&cpu, 0x3000 - 4 - 60), 0x1F38_0000);
}

#[test]
fn frestore_of_a_null_frame_goes_back_to_null() {
    // FNOP ; FRESTORE (A0)+ ; FSAVE -(A0)
    let mut cpu = testcpu(&[0xF280, 0x0000, 0xF358, 0xF320]);
    cpu.step().unwrap();
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.a[0], 0x3000);
    assert_eq!(read_long(&cpu, 0x3000), 0);
}

#[test]
fn fmod_and_frem() {
    // dest, src, opmode, result, quotient, sign of the quotient
    for (d, m, opmode, r, q, qs) in [
        (-10.0, 3.0, 0x21, -1.0, 3, true),
        (10.0, 3.0, 0x21, 1.0, 3, false),
        (10.0, 3.0, 0x25, 1.0, 3, false),
        (11.0, 3.0, 0x25, -1.0, 4, false),
        (-11.0, 3.0, 0x25, 1.0, 4, true),
        (5.0, 2.0, 0x25, 1.0, 2, false),
        (7.0, 2.0, 0x25, -1.0, 4, false),
        (1.0, 3.0, 0x21, 1.0, 0, false),
        (2f64.powi(70), 3.0, 0x21, 1.0, 0, false),
    ] {
        let mut cpu = testcpu(&fop(0, 1, opmode));
        cpu.regs.fpu.fp[0] = fp(m);
        cpu.regs.fpu.fp[1] = fp(d);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.fpu.fp[1].as_f64(), r, "{d} {m} {opmode:02X}");
        assert_eq!(cpu.regs.fpu.fpsr.quotient_s(), qs, "{d} {m} {opmode:02X}");
        if d.abs() < 1e6 {
            assert_eq!(cpu.regs.fpu.fpsr.quotient(), q, "{d} {m} {opmode:02X}");
        }
    }
}

#[test]
fn fmod_large_quotient_low_bits() {
    // 2^70 / 3 is 0x1555...5 with the low bits 1010101
    let mut cpu = testcpu(&fop(0, 1, 0x21));
    cpu.regs.fpu.fp[0] = fp(3.0);
    cpu.regs.fpu.fp[1] = fp(2f64.powi(70));
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpsr.quotient(), 0x55);
}

#[test]
fn fmod_invalid_is_operr() {
    let mut cpu = testcpu(&fop(0, 1, 0x21));
    cpu.regs.fpu.fp[0] = fp(0.0);
    cpu.regs.fpu.fp[1] = fp(1.0);
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.fp[1].is_nan());
    assert!(cpu.regs.fpu.fpsr.exs().operr());
}

#[test]
fn fgetman_keeps_the_sign() {
    for (v, want) in [(-6.0, -1.5), (6.0, 1.5), (1.0, 1.0)] {
        let mut cpu = testcpu(&fop(0, 1, 0x1F));
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.fpu.fp[1].as_f64(), want, "{v}");
    }
    let mut cpu = testcpu(&fop(0, 1, 0x1F));
    cpu.regs.fpu.fp[0] = BitsExtReal::zero(true);
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.fp[1].is_zero() && cpu.regs.fpu.fp[1].s());
}

#[test]
fn fgetexp_of_zero_is_zero() {
    for (v, want) in [(8.0, 3.0), (0.5, -1.0), (0.0, 0.0)] {
        let mut cpu = testcpu(&fop(0, 1, 0x1E));
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.step().unwrap();
        assert_eq!(cpu.regs.fpu.fp[1].as_f64(), want, "{v}");
    }
}

#[test]
fn fgetexp_fgetman_of_infinity_are_operr() {
    for opmode in [0x1E, 0x1F] {
        let mut cpu = testcpu(&fop(0, 1, opmode));
        cpu.regs.fpu.fp[0] = BitsExtReal::inf(false);
        cpu.step().unwrap();
        assert!(cpu.regs.fpu.fp[1].is_nan());
        assert!(cpu.regs.fpu.fpsr.exs().operr());
    }
}

#[test]
fn fetoxm1_flognp1_keep_small_values() {
    for (opmode, v) in [(0x08, 1e-20), (0x06, 1e-20), (0x08, 1e-30), (0x06, -1e-30)] {
        let mut cpu = testcpu(&fop(0, 1, opmode));
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.step().unwrap();
        let r = cpu.regs.fpu.fp[1].as_f64();
        assert!(
            (r - v).abs() <= v.abs() * 1e-15,
            "{opmode:02X} {v} gave {r}"
        );
    }
}

#[test]
fn fetoxm1_flognp1_usual_values() {
    for (opmode, v, want) in [
        (0x08, 1.0, std::f64::consts::E - 1.0),
        (0x06, 1.0, std::f64::consts::LN_2),
        (0x08, -1.0, 1.0 / std::f64::consts::E - 1.0),
    ] {
        let mut cpu = testcpu(&fop(0, 1, opmode));
        cpu.regs.fpu.fp[0] = fp(v);
        cpu.step().unwrap();
        let r = cpu.regs.fpu.fp[1].as_f64();
        assert!((r - want).abs() < 1e-14, "{opmode:02X} {v} gave {r}");
    }
}

#[test]
fn fpiar_has_the_last_instruction_that_can_raise() {
    // FADD FP0,FP1 ; FMOVE.L #1,FPSR ; FBT
    let mut cpu = testcpu(&[0xF200, 0x00A2, 0xF23C, 0x8800, 0, 1, 0xF28F, 0x0000]);
    cpu.regs.fpu.fp[0] = fp(1.0);
    cpu.regs.fpu.fp[1] = fp(1.0);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpiar, CODE);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpiar, CODE);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpiar, CODE + 12);
}

#[test]
fn fpcr_fpsr_unimplemented_bits_read_as_zero() {
    // FMOVE.L D0,FPCR ; FMOVE.L D0,FPSR
    let mut cpu = testcpu(&[0xF200, 0x9000, 0xF200, 0x8800]);
    cpu.regs.d[0] = 0xFFFF_FFFF;
    cpu.step().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.fpu.fpcr.0, 0x0000_FFF0);
    assert_eq!(cpu.regs.fpu.fpsr.0, 0x0FFF_FFF8);
}

#[test]
fn fmovecr_sets_the_condition_codes() {
    let mut cpu = testcpu(&[0xF200, 0x5C0F]); // FMOVECR #$0F,FP0
    cpu.regs.fpu.fpsr.set_fpcc_n(true);
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.fpsr.fpcc_z());
    assert!(!cpu.regs.fpu.fpsr.fpcc_n());
}

#[test]
fn reset_instruction_resets_the_fpu() {
    let mut cpu = testcpu(&[0x4E70]);
    cpu.regs.fpu.fp[0] = fp(1.0);
    cpu.regs.fpu.fpcr.0 = 0x10;
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.fp[0].is_nan());
    assert_eq!(cpu.regs.fpu.fpcr.0, 0);
}

#[test]
fn transcendental_functions_in_every_rounding_mode() {
    // Only the final result gets the rounding mode (MC68881UM 4.5.5.2)
    const OPMODES: [Word; 19] = [
        0b0001110, 0b0001111, 0b0001100, 0b0011100, 0b0001010, 0b0000010, 0b0011001, 0b0001001,
        0b0001101, 0b0010000, 0b0001000, 0b0010001, 0b0010010, 0b0010100, 0b0000110, 0b0010101,
        0b0010110, 0b0011110, 0b0011111,
    ];
    for rnd in 0..4 {
        for opmode in OPMODES {
            let mut cpu = testcpu(&fop(1, 0, opmode));
            cpu.regs.fpu.fpcr.set_rnd(rnd);
            cpu.regs.fpu.fp[1] = fp(0.25);
            cpu.step().unwrap();
            assert!(
                !cpu.regs.fpu.fp[0].is_nan(),
                "opmode {opmode:07b} mode {rnd}"
            );
        }
        // FSINCOS FP1,FP2:FP0
        let mut cpu = testcpu(&fop(1, 0, 0b0110010));
        cpu.regs.fpu.fpcr.set_rnd(rnd);
        cpu.regs.fpu.fp[1] = fp(0.25);
        cpu.step().unwrap();
        assert!(!cpu.regs.fpu.fp[0].is_nan());
        assert!(!cpu.regs.fpu.fp[2].is_nan());
    }
}

#[test]
fn binary_real_sources_become_extended_exactly() {
    // MC68881UM tables 3-1 to 3-3: (raw single, sign and exponent, mantissa)
    for (raw, high, man) in [
        (0x3F80_0000, 0x3FFF_0000, 0x8000_0000_0000_0000),
        (0xBFC0_0000, 0xBFFF_0000, 0xC000_0000_0000_0000),
        // Denormalized in single precision, normalized in extended precision
        (0x0000_0001, 0x3F6A_0000, 0x8000_0000_0000_0000),
        (0x007F_FFFF, 0x3F80_0000, 0xFFFF_FE00_0000_0000),
        (0x7F80_0000, 0x7FFF_0000, 0),
        // Signaling and non-signaling NANs stay what they are
        (0x7FA0_0001, 0x7FFF_0000, 0xA000_0100_0000_0000),
        (0xFFC0_0000, 0xFFFF_0000, 0xC000_0000_0000_0000),
    ] {
        let bits = BitsExtReal::from_single(raw);
        assert_eq!((bits.high(), bits.raw_mantissa()), (high, man), "{raw:08X}");
    }
    for (raw, high, man) in [
        (0x3FF0_0000_0000_0000, 0x3FFF_0000, 0x8000_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x3BCD_0000, 0x8000_0000_0000_0000),
        (0xFFF0_0000_0000_0000, 0xFFFF_0000, 0),
        (0x7FF4_0000_0000_0000, 0x7FFF_0000, 0xA000_0000_0000_0000),
    ] {
        let bits = BitsExtReal::from_double(raw);
        assert_eq!(
            (bits.high(), bits.raw_mantissa()),
            (high, man),
            "{raw:016X}"
        );
    }
}

/// A value by its bits: sign and exponent, and mantissa
fn ext(high: Word, man: u64) -> BitsExtReal {
    BitsExtReal::default()
        .with_high(Long::from(high) << 16)
        .with_raw_mantissa(man)
}

/// Runs FOP FP1,FP0 with FP0 = `dest` and FP1 = `src` and the mode bits of FPCR
fn run_fop(opmode: Word, dest: BitsExtReal, src: BitsExtReal, fpcr: Long) -> TestCpu {
    let mut cpu = testcpu(&fop(1, 0, opmode));
    cpu.regs.fpu.fpcr.0 = fpcr;
    cpu.regs.fpu.fp[0] = dest;
    cpu.regs.fpu.fp[1] = src;
    cpu.step().unwrap();
    cpu
}

const J: u64 = 1 << 63;
const FMUL: Word = 0x23;
const FDIV: Word = 0x20;
const FADD: Word = 0x22;
const FMOVE: Word = 0x00;
const FSQRT: Word = 0x04;
const OVFL: Long = 1 << 12;
const UNFL: Long = 1 << 11;
const INEX2: Long = 1 << 9;

/// The value and the exception and accrued bytes of a result
fn result(cpu: &TestCpu) -> (Long, u64, Long) {
    let fp0 = cpu.regs.fpu.fp[0];
    (
        fp0.high() >> 16,
        fp0.raw_mantissa(),
        cpu.regs.fpu.fpsr.0 & 0xFFFF,
    )
}

/// Exponent 0 is a weight like any other: (-1)^s x 2^(e-16383) x j.f, and
/// denormalized numbers are that with j = 0 (MC68881UM table 3-3)
#[test]
fn denormalized_numbers_have_the_weight_of_exponent_0() {
    // 0x7FFFFFFF x 2^-16390 from Motorola's ftest, the source being denormalized
    let cpu = run_fop(
        FMUL,
        ext(0x401D, 0xFFFF_FFFE_0000_0000),
        ext(0x8000, 0x0100_0000_0000_0000),
        0,
    );
    assert_eq!(result(&cpu), (0x8017, 0xFFFF_FFFE_0000_0000, 0));
    // The smallest normalized number is the one with exponent 0: half of it is
    // denormalized, and exactly representable. So UNFL, and no INEX2 in either byte,
    // which keeps AUNFL clear as well (MC68881UM 6.1.5, Motorola's ftest)
    let cpu = run_fop(FDIV, ext(0, J), ext(0x4000, J), 0);
    assert_eq!(result(&cpu), (0, 0x4000_0000_0000_0000, UNFL));
}

/// Overflow is checked after rounding, and only sets INEX2 if the rounding was not
/// exact (MC68881UM 4.5.5.2, figure 6-3, Motorola's ftest). The accrued byte has
/// AOVFL, and AINEX from the overflow alone (MC68881UM 2.3.4).
#[test]
fn overflow_results_by_rounding_mode() {
    const AOVFL_AINEX: Long = (1 << 6) | (1 << 3);
    const INF: (Long, u64) = (0x7FFF, 0);
    const NEG_INF: (Long, u64) = (0xFFFF, 0);
    const LARGEST: (Long, u64) = (0x7FFE, !0);
    const NEG_LARGEST: (Long, u64) = (0xFFFE, !0);
    // MC68881UM 6.1.4, trap disabled results
    for (rnd, positive, negative) in [
        (0, INF, NEG_INF),
        (1, LARGEST, NEG_LARGEST),
        (2, LARGEST, NEG_INF),
        (3, INF, NEG_LARGEST),
    ] {
        let fpcr = rnd << 4;
        let cpu = run_fop(FMUL, ext(0x4000, J), ext(0x7FFE, J), fpcr);
        assert_eq!(
            result(&cpu),
            (positive.0, positive.1, OVFL | AOVFL_AINEX),
            "mode {rnd}"
        );
        let cpu = run_fop(FMUL, ext(0xC000, J), ext(0x7FFE, J), fpcr);
        assert_eq!(
            result(&cpu),
            (negative.0, negative.1, OVFL | AOVFL_AINEX),
            "mode {rnd}"
        );
    }
    // Rounding up into overflow: inexact as well
    let cpu = run_fop(FMOVE, ext(0, 0), ext(0x407E, !0), 1 << 6);
    assert_eq!(result(&cpu), (0x7FFF, 0, OVFL | INEX2 | AOVFL_AINEX));
}

/// In single or double precision, overflow and underflow go by the range of that
/// format, in the extended format of the register (MC68881UM 6.1.4, 6.1.5, 6.1.7)
#[test]
fn range_of_the_rounding_precision() {
    const AINEX: Long = 1 << 3;
    const AUNFL: Long = 1 << 5;
    const SINGLE: Long = 1 << 6;
    const DOUBLE: Long = 2 << 6;
    const RM: Long = 2 << 4;
    // Largest single precision number, from MC68881UM 6.1.7: exponent $00FF unbiased
    let cpu = run_fop(FMOVE, ext(0, 0), ext(0x4100, J), SINGLE | RM);
    assert_eq!(
        result(&cpu),
        (0x407E, 0xFFFF_FF00_0000_0000, OVFL | (1 << 6) | AINEX)
    );
    let cpu = run_fop(FMOVE, ext(0, 0), ext(0x4400, J), DOUBLE | RM);
    assert_eq!(
        result(&cpu),
        (0x43FE, 0xFFFF_FFFF_FFFF_F800, OVFL | (1 << 6) | AINEX)
    );
    // Half the smallest normalized single precision number stays exact, UNFL alone
    let cpu = run_fop(FMOVE, ext(0, 0), ext(0x3F80, J), SINGLE);
    assert_eq!(result(&cpu), (0x3F80, J, UNFL));
    // Below it the mantissa has fewer bits: 1.5 x 2^-149 rounds to 2 x 2^-149
    let cpu = run_fop(FMOVE, ext(0, 0), ext(0x3F6A, 0xC000_0000_0000_0000), SINGLE);
    assert_eq!(result(&cpu), (0x3F6B, J, UNFL | INEX2 | AUNFL | AINEX));
    // All of it goes: the rounding mode says what stays (MC68881UM 6.1.5)
    for (rnd, high, man) in [(0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0x3F6A, J)] {
        let cpu = run_fop(FMOVE, ext(0, 0), ext(0x3000, J), SINGLE | (rnd << 4));
        assert_eq!(
            result(&cpu),
            (high, man, UNFL | INEX2 | AUNFL | AINEX),
            "mode {rnd}"
        );
    }
}

/// Rounding happens once, from all the bits (MC68881UM figure 6-3): rounding to
/// extended precision first would lose the sticky bit here
#[test]
fn no_double_rounding() {
    const AINEX: Long = 1 << 3;
    // 1 + 2^-24 + 2^-16382: above half way in single precision
    let cpu = run_fop(
        FADD,
        ext(0x3FFF, 0x8000_0080_0000_0000),
        ext(0x0001, J),
        1 << 6,
    );
    assert_eq!(result(&cpu), (0x3FFF, 0x8000_0100_0000_0000, INEX2 | AINEX));
}

/// INEX2 for anything lost, however far apart the operands are (MC68881UM figure 6-3)
#[test]
fn inexact_with_operands_far_apart() {
    const AINEX: Long = 1 << 3;
    let cpu = run_fop(FADD, ext(0x7FFE, !0), ext(0x3FFF, J), 0);
    assert_eq!(result(&cpu), (0x7FFE, !0, INEX2 | AINEX));
    // Toward zero, a smaller number of the other sign takes one off
    let cpu = run_fop(FADD, ext(0x3FFF, J), ext(0x8000, 1), 1 << 4);
    assert_eq!(result(&cpu), (0x3FFE, !0, INEX2 | AINEX));
}

/// Rounds correctly: sqrt(1 - 2^-64) is nearer to 1 - 2^-64 than to 1
#[test]
fn fsqrt_rounds_correctly() {
    const AINEX: Long = 1 << 3;
    let cpu = run_fop(FSQRT, ext(0, 0), ext(0x3FFE, !0), 0);
    assert_eq!(result(&cpu), (0x3FFE, !0, INEX2 | AINEX));
    let cpu = run_fop(FSQRT, ext(0, 0), ext(0x4001, J), 0);
    assert_eq!(result(&cpu), (0x4000, J, 0));
    let cpu = run_fop(FSQRT, ext(0, 0), ext(0x4000, J), 0);
    assert_eq!(result(&cpu), (0x3FFF, 0xB504_F333_F9DE_6484, INEX2 | AINEX));
}

/// NANs: the operand itself is the result, the destination's if both are, and a
/// signaling one becomes non-signaling (MC68881UM 4.5.4). Created NANs have all ones,
/// and the sign is clear in what the 68881UM calls the default NAN (table 3-3, Motorola's
/// ftest).
#[test]
fn nan_results() {
    const QNAN: BitsExtReal = BitsExtReal(0xFFFF_0000_D555_5555_5555_5555);
    const SNAN_BIT: Long = 1 << 14;
    const AIOP: Long = 1 << 7;
    const OPERR: Long = 1 << 13;
    let snan = ext(0x7FFF, 0xA000_0000_0000_0000);
    // Propagated with the payload, and no exception
    let cpu = run_fop(FADD, ext(0x3FFF, J), QNAN, 0);
    assert_eq!(result(&cpu), (0xFFFF, 0xD555_5555_5555_5555, 0));
    // FNEG and FABS change nothing of it
    let cpu = run_fop(0x1A, ext(0, 0), QNAN, 0);
    assert_eq!(result(&cpu), (0xFFFF, 0xD555_5555_5555_5555, 0));
    // The destination's has priority
    let cpu = run_fop(FADD, ext(0x7FFF, 0xC000_0000_0000_0000), QNAN, 0);
    assert_eq!(result(&cpu), (0x7FFF, 0xC000_0000_0000_0000, 0));
    // Signaling
    let cpu = run_fop(FADD, ext(0x3FFF, J), snan, 0);
    assert_eq!(
        result(&cpu),
        (0x7FFF, 0xE000_0000_0000_0000, SNAN_BIT | AIOP)
    );
    // Created
    let cpu = run_fop(FDIV, ext(0, 0), ext(0, 0), 0);
    assert_eq!(result(&cpu), (0x7FFF, !0, OPERR | AIOP));
}

/// FINT and FGETMAN give extended precision numbers (MC68881UM FINT, FGETMAN), and the
/// rounding precision of FPCR doesn't apply to them
#[test]
fn extended_results_whatever_the_rounding_precision() {
    const SINGLE: Long = 1 << 6;
    let value = ext(0x7FFE, !0);
    assert_eq!(
        result(&run_fop(0x01, ext(0, 0), value, SINGLE)),
        (0x7FFE, !0, 0)
    );
    assert_eq!(
        result(&run_fop(0x03, ext(0, 0), value, SINGLE)),
        (0x7FFE, !0, 0)
    );
    assert_eq!(
        result(&run_fop(0x1F, ext(0, 0), value, SINGLE)),
        (0x3FFF, !0, 0)
    );
}

/// FSGLMUL and FSGLDIV truncate the mantissas of the operands to single precision,
/// round the result to it, and keep the exponent range of extended precision
/// (MC68881UM 4.5.5.2)
#[test]
fn single_precision_multiply_and_divide() {
    const FSGLMUL: Word = 0x27;
    const FSGLDIV: Word = 0x24;
    // The bits below 24 of the operand don't count
    let cpu = run_fop(FSGLMUL, ext(0x3FFF, J | 0xFF_FFFF_FFFF), ext(0x3FFF, J), 0);
    assert_eq!(result(&cpu), (0x3FFF, J, 0));
    // Far out of the range of single precision, and of any FPCR precision
    let cpu = run_fop(FSGLMUL, ext(0x7000, J), ext(0x3FFF, J), 2 << 6);
    assert_eq!(result(&cpu), (0x7000, J, 0));
    let cpu = run_fop(
        FSGLDIV,
        ext(0x3FFF, J),
        ext(0x4000, 0xC000_0000_0000_0000),
        0,
    );
    assert_eq!(
        result(&cpu),
        (0x3FFD, 0xAAAA_AB00_0000_0000, INEX2 | (1 << 3))
    );
}

/// From 2^14 on, FSCALE always overflows or underflows (MC68881UM FSCALE), here with a
/// denormalized number that would otherwise come back in range
#[test]
fn fscale_out_of_range() {
    const FSCALE: Word = 0x26;
    const AOVFL_AINEX: Long = (1 << 6) | (1 << 3);
    let cpu = run_fop(FSCALE, ext(0, 1), ext(0x400D, J), 0);
    assert_eq!(result(&cpu), (0x7FFF, 0, OVFL | AOVFL_AINEX));
    let cpu = run_fop(FSCALE, ext(0x7FFE, !0), ext(0xC00D, J), 0);
    assert_eq!(result(&cpu), (0, 0, UNFL | INEX2 | (1 << 5) | (1 << 3)));
    // In range below that
    let cpu = run_fop(
        FSCALE,
        ext(0x3FFF, J),
        ext(0x4000, 0xC000_0000_0000_0000),
        0,
    );
    assert_eq!(result(&cpu), (0x4002, J, 0));
    // By an infinity is an operand error, and of an infinity or a zero the same
    let cpu = run_fop(FSCALE, ext(0x3FFF, J), BitsExtReal::inf(false), 0);
    assert_eq!(result(&cpu), (0x7FFF, !0, (1 << 13) | (1 << 7)));
    let cpu = run_fop(FSCALE, BitsExtReal::inf(true), ext(0x3FFF, J), 0);
    assert_eq!(result(&cpu), (0xFFFF, 0, 0));
}
