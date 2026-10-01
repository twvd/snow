//! 68020 long division (DIVS.L/DIVU.L) tests

use crate::bus::Address;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::Long;

type TestCpu = CpuM68020Fpu<Testbus<Address, u8>>;

const SIGNED: u16 = 0x0800;
const SIZE_64: u16 = 0x0400;

/// Runs DIVx.L D2,D1:D0 (or DIVx.L D2,D0 without `SIZE_64`) and returns the CPU
fn divl(flags: u16, d1: Long, d0: Long, d2: Long) -> TestCpu {
    const PC: Address = 0x1000;
    let ext = flags | if flags & SIZE_64 != 0 { 1 } else { 0 };
    let bus = Testbus::new(M68020_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);

    for (i, word) in [0x4C42, ext, 0x4E71, 0x4E71].into_iter().enumerate() {
        let addr = PC + (i as Address * 2);
        cpu.bus.mem.insert(addr, (word >> 8) as u8);
        cpu.bus.mem.insert(addr + 1, word as u8);
    }

    cpu.regs.isp = 0x800;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.set_pc(PC).expect("set_pc failed");
    cpu.prefetch_refill().expect("prefetch_refill failed");

    cpu.regs.write_d(0, d0);
    cpu.regs.write_d(1, d1);
    cpu.regs.write_d(2, d2);
    cpu.step().unwrap();
    cpu
}

fn regs(cpu: &TestCpu) -> (Long, Long) {
    (cpu.regs.read_d(1), cpu.regs.read_d(0))
}

#[test]
fn divs_l_32_min_by_minus_one_overflows() {
    let cpu = divl(SIGNED, 0x1234_5678, 0x8000_0000, 0xFFFF_FFFF);
    assert!(cpu.regs.sr.v());
    assert_eq!(regs(&cpu), (0x1234_5678, 0x8000_0000));
}

#[test]
fn divs_l_32() {
    let cpu = divl(SIGNED, 0, -7i32 as Long, 2);
    assert!(!cpu.regs.sr.v());
    assert!(cpu.regs.sr.n());
    assert_eq!(cpu.regs.read_d::<Long>(0), -3i32 as Long);
}

#[test]
fn divs_l_64_negative_divisor() {
    // 0x1_0000_0000 / -2 = -0x8000_0000
    let cpu = divl(SIGNED | SIZE_64, 1, 0, -2i32 as Long);
    assert!(!cpu.regs.sr.v());
    assert!(cpu.regs.sr.n());
    assert_eq!(regs(&cpu), (0, 0x8000_0000));

    // -7 / -2 = 3, remainder -1
    let cpu = divl(SIGNED | SIZE_64, 0xFFFF_FFFF, -7i32 as Long, -2i32 as Long);
    assert!(!cpu.regs.sr.v());
    assert_eq!(regs(&cpu), (0xFFFF_FFFF, 3));
}

#[test]
fn divs_l_64_overflow() {
    // 0x1_0000_0000 / 2 = 0x8000_0000 doesn't fit a signed long
    let cpu = divl(SIGNED | SIZE_64, 1, 0, 2);
    assert!(cpu.regs.sr.v());
    assert_eq!(regs(&cpu), (1, 0));

    // i64::MIN / -1
    let cpu = divl(SIGNED | SIZE_64, 0x8000_0000, 0, 0xFFFF_FFFF);
    assert!(cpu.regs.sr.v());
    assert_eq!(regs(&cpu), (0x8000_0000, 0));
}

#[test]
fn divu_l_64() {
    // 0x1_0000_0000 / 2 = 0x8000_0000
    let cpu = divl(SIZE_64, 1, 0, 2);
    assert!(!cpu.regs.sr.v());
    assert_eq!(regs(&cpu), (0, 0x8000_0000));
}

#[test]
fn divu_l_64_overflow() {
    // 0xFFFF_FFFF_0000_0000 / 1: the quotient's high long is all ones
    let cpu = divl(SIZE_64, 0xFFFF_FFFF, 0, 1);
    assert!(cpu.regs.sr.v());
    assert_eq!(regs(&cpu), (0xFFFF_FFFF, 0));
}
