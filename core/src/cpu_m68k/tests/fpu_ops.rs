//! 6888x behaviour from the MC68881/MC68882 User's Manual

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
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

fn fp(v: f64) -> Float {
    Float::from_f64(v).cast(SEMANTICS_EXTENDED)
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
    let inf = |s| Float::inf(SEMANTICS_EXTENDED, s);
    let zero = |s| Float::zero(SEMANTICS_EXTENDED, s);
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
    let big = (0..9).fold(fp(1e300), |v, _| v * fp(1e300));
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
        (big.clone(), big, 0x23, false, false, true, true),
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
    cpu.regs.fpu.fp[0] = Float::zero(SEMANTICS_EXTENDED, true);
    cpu.step().unwrap();
    assert!(cpu.regs.fpu.fp[1].is_zero() && cpu.regs.fpu.fp[1].is_negative());
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
        cpu.regs.fpu.fp[0] = Float::inf(SEMANTICS_EXTENDED, false);
        cpu.step().unwrap();
        assert!(cpu.regs.fpu.fp[1].is_nan());
        assert!(cpu.regs.fpu.fpsr.exs().operr());
    }
}
