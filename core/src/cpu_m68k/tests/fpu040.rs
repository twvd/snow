//! Tests for the M68040 FPU

use arpfloat::Float;

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::instruction::opmode;
use crate::cpu_m68k::{CpuM68040Fpu, M68040_ADDRESS_MASK};

type TestCpu = CpuM68040Fpu<Testbus<Address, u8, Bus68020>>;

fn ext(v: f64) -> Float {
    Float::from_f64(v).cast(SEMANTICS_EXTENDED)
}

#[test]
fn rounded_add() {
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK));
    // Exact in double precision, not in single
    let small = ext(2f64.powi(-30));
    let one = ext(1.0);

    let r = cpu.fpu_alu_op(opmode::FSADD, &small, &one).unwrap();
    assert_eq!(r.as_f64(), 1.0);
    let r = cpu.fpu_alu_op(opmode::FDADD, &small, &one).unwrap();
    assert_eq!(r.as_f64(), 1.0 + 2f64.powi(-30));
    let r = cpu.fpu_alu_op(opmode::FADD, &small, &one).unwrap();
    assert_eq!(r.as_f64(), 1.0 + 2f64.powi(-30));
}
