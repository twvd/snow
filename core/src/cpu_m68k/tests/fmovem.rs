//! FMOVEM of the floating-point data registers

use arpfloat::Float;

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::fpu::SEMANTICS_EXTENDED;
use crate::cpu_m68k::fpu::storage::BitsExtReal;
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
        cpu.regs.fpu.fp[n] = BitsExtReal::from(&Float::from_u64(SEMANTICS_EXTENDED, n as u64 + 1));
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

#[test]
fn dynamic_postincrement_load() {
    // FMOVEM.X (A0)+,D1 with FP0/FP2 in D1
    let cpu = run(0xF218, 0xD810, |cpu| {
        cpu.regs.write_d::<Long>(1, 0xA0);
        set_ext(cpu, DATA, THREE);
        set_ext(cpu, DATA + 12, ONE);
    });
    assert_eq!(cpu.regs.fpu.fp[0].as_f64(), 3.0);
    assert_eq!(cpu.regs.fpu.fp[2].as_f64(), 1.0);
    assert_eq!(cpu.regs.read_a::<Address>(0), DATA + 24);
}

#[test]
fn dynamic_predecrement_store() {
    // FMOVEM.X D1,-(A0) with FP0/FP2 in D1
    let cpu = run(0xF220, 0xE810, |cpu| cpu.regs.write_d::<Long>(1, 0x05));
    let base = DATA - 24;
    assert_eq!(cpu.regs.read_a::<Address>(0), base);
    assert_eq!(ext_at(&cpu, base), ONE);
    assert_eq!(ext_at(&cpu, base + 12), THREE);
}

/// FMOVEM moves the registers with no conversion (MC68881UM FMOVEM), so whatever
/// the bits are they come back, but for the unused ones of the format, a don't care
/// on input and zero on output (MC68881UM table 3-3)
#[test]
fn moves_all_the_bits_of_a_register() {
    const VALUES: [[Long; 3]; 7] = [
        // Non-signaling NAN with a payload
        [0xFFFF_0000, 0xD555_5555, 0x5555_5555],
        // Signaling NAN
        [0x7FFF_0000, 0xA000_0000, 0],
        // Infinity with either integer bit
        [0x7FFF_0000, 0, 0],
        [0x7FFF_0000, 0x8000_0000, 0],
        // Denormalized
        [0x0000_0000, 0x0000_0000, 1],
        // Exponent 0 with the integer bit set
        [0x8000_0000, 0x8000_0000, 0],
        // Unnormalized
        [0x3FFF_0000, 0x4000_0000, 0],
    ];
    for [high, mid, low] in VALUES {
        // FMOVEM.X (A0),FP0
        let cpu = run(0xF210, 0xD080, |cpu| {
            for (i, long) in [high | 0x1234, mid, low].into_iter().enumerate() {
                for (j, byte) in long.to_be_bytes().into_iter().enumerate() {
                    cpu.bus
                        .mem
                        .insert(DATA + 4 * i as Address + j as Address, byte);
                }
            }
        });
        let fp0 = cpu.regs.fpu.fp[0];
        assert_eq!([fp0.high(), fp0.mid(), fp0.low()], [high, mid, low]);

        // FMOVEM.X FP0,(A0)
        let cpu = run(0xF210, 0xF080, |cpu| cpu.regs.fpu.fp[0] = fp0);
        assert_eq!(ext_at(&cpu, DATA), (high, mid));
        assert_eq!(ext_at(&cpu, DATA + 4), (mid, low));
    }
}
