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
