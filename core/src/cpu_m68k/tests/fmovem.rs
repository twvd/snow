//! FMOVEM of the floating-point data registers

use arpfloat::Float;

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const DATA: Address = 0x3000;

/// Runs `opcode`, `extword` with A0 = DATA and FPn = n + 1
fn run(opcode: Word, extword: Word, setup: impl Fn(&mut TestCpu)) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68020_ADDRESS_MASK));
    for (i, w) in [opcode, extword, 0x4E71, 0x4E71].into_iter().enumerate() {
        let addr = CODE + 2 * i as Address;
        cpu.bus.mem.insert(addr, (w >> 8) as u8);
        cpu.bus.mem.insert(addr + 1, w as u8);
    }
    cpu.regs.isp = 0x800;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.write_a(0, DATA);
    for n in 0..8 {
        cpu.regs.fpu.fp[n] = Float::from_u64(SEMANTICS_EXTENDED, n as u64 + 1);
    }
    setup(&mut cpu);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 4);
    cpu
}

/// The high long word (sign and exponent) and the mantissa's high long word
/// of the extended precision value at `addr`
fn ext_at(cpu: &TestCpu, addr: Address) -> (Long, Long) {
    let long = |a: Address| {
        Long::from_be_bytes(core::array::from_fn(|i| cpu.bus.mem[&(a + i as Address)]))
    };
    (long(addr), long(addr + 4))
}

const ONE: (Long, Long) = (0x3FFF_0000, 0x8000_0000);
const THREE: (Long, Long) = (0x4000_0000, 0xC000_0000);

fn set_ext(cpu: &mut TestCpu, addr: Address, (high, mid): (Long, Long)) {
    for (i, b) in high
        .to_be_bytes()
        .into_iter()
        .chain(mid.to_be_bytes())
        .chain([0; 4])
        .enumerate()
    {
        cpu.bus.mem.insert(addr + i as Address, b);
    }
}

#[test]
fn control_store() {
    // FMOVEM.X FP0/FP2,(A0): FP0 in bit 7
    let cpu = run(0xF210, 0xF0A0, |_| ());
    assert_eq!(ext_at(&cpu, DATA), ONE);
    assert_eq!(ext_at(&cpu, DATA + 12), THREE);
}

#[test]
fn predecrement_store() {
    // FMOVEM.X FP0/FP2,-(A0): FP0 in bit 0, still at the lower address
    let cpu = run(0xF220, 0xE005, |_| ());
    let base = DATA - 24;
    assert_eq!(cpu.regs.read_a::<Address>(0), base);
    assert_eq!(ext_at(&cpu, base), ONE);
    assert_eq!(ext_at(&cpu, base + 12), THREE);
}

#[test]
fn control_load() {
    // FMOVEM.X (A0),FP1/FP3
    let cpu = run(0xF210, 0xD050, |cpu| {
        set_ext(cpu, DATA, ONE);
        set_ext(cpu, DATA + 12, THREE);
    });
    assert_eq!(cpu.regs.fpu.fp[1].as_f64(), 1.0);
    assert_eq!(cpu.regs.fpu.fp[3].as_f64(), 3.0);
}
