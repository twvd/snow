//! CMP2 and CHK2 (68020 and later)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::{Byte, Long, Word};

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const BOUNDS: Address = 0x3000;
const STACK: Address = 0x800;
const CHK_HANDLER: Address = 0x2000;
const ILLEGAL_HANDLER: Address = 0x2100;

fn write(cpu: &mut TestCpu, addr: Address, bytes: &[Byte]) {
    for (i, &b) in bytes.iter().enumerate() {
        cpu.bus.mem.insert(addr + i as Address, b);
    }
}

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    write(cpu, addr, &value.to_be_bytes());
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    Long::from_be_bytes(core::array::from_fn(|i| {
        cpu.bus.mem[&(addr + i as Address)]
    }))
}

/// Runs the instruction of `code` with `bounds` at BOUNDS in A0
fn run(code: &[Word], bounds: &[Byte], setup: impl Fn(&mut TestCpu)) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68020_ADDRESS_MASK));
    for (i, &w) in code.iter().chain([0x4E71, 0x4E71].iter()).enumerate() {
        write_word(&mut cpu, CODE + 2 * i as Address, w);
    }
    write(&mut cpu, BOUNDS, bounds);
    write(&mut cpu, 0x18, &CHK_HANDLER.to_be_bytes());
    write(&mut cpu, 0x10, &ILLEGAL_HANDLER.to_be_bytes());
    cpu.regs.isp = STACK;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.regs.write_a(0, BOUNDS);
    setup(&mut cpu);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    cpu
}

/// CMP2.B (A0),D0 with D0 = `d0`: (Z, C)
fn cmp2_b(bounds: [Byte; 2], d0: Long) -> (bool, bool) {
    let cpu = run(&[0x00D0, 0x0000], &bounds, |cpu| cpu.regs.write_d(0, d0));
    assert_eq!(cpu.regs.pc, CODE + 4);
    (cpu.regs.sr.z(), cpu.regs.sr.c())
}

#[test]
fn signed_bounds() {
    // -5 to 5
    let bounds = [0xFB, 0x05];
    assert_eq!(cmp2_b(bounds, 3), (false, false));
    assert_eq!(cmp2_b(bounds, 0xFB), (true, false));
    assert_eq!(cmp2_b(bounds, 5), (true, false));
    assert_eq!(cmp2_b(bounds, 6), (false, true));
    assert_eq!(cmp2_b(bounds, 0xFA), (false, true));
    assert_eq!(cmp2_b(bounds, 0x80), (false, true));
    // Only the low byte of a data register
    assert_eq!(cmp2_b(bounds, 0x1234_5603), (false, false));
}

#[test]
fn unsigned_bounds() {
    // 10 to 200
    let bounds = [10, 200];
    assert_eq!(cmp2_b(bounds, 100), (false, false));
    assert_eq!(cmp2_b(bounds, 200), (true, false));
    assert_eq!(cmp2_b(bounds, 201), (false, true));
    assert_eq!(cmp2_b(bounds, 5), (false, true));
}

#[test]
fn address_register() {
    // CMP2.W (A0),A1: bounds -16 to 16, sign-extended, against all of A1
    let bounds = [0xFF, 0xF0, 0x00, 0x10];
    let flags = |a1: Address| {
        let cpu = run(&[0x02D0, 0x9000], &bounds, |cpu| cpu.regs.write_a(1, a1));
        (cpu.regs.sr.z(), cpu.regs.sr.c())
    };
    assert_eq!(flags(0xFFFF_FFF8), (false, false));
    assert_eq!(flags(0x0000_FFF8), (false, true));
    assert_eq!(flags(0xFFFF_FFF0), (true, false));
}

#[test]
fn pc_relative() {
    // CMP2.L (d16,PC),D0, the bounds 0x100 bytes after the displacement word
    let cpu = run(&[0x04FA, 0x0000, 0x0100], &[], |cpu| {
        write(cpu, CODE + 4 + 0x100, &[0, 0, 0, 1, 0, 0, 0, 9]);
        cpu.regs.write_d::<Long>(0, 9);
    });
    assert_eq!(cpu.regs.pc, CODE + 6);
    assert!(cpu.regs.sr.z());
    assert!(!cpu.regs.sr.c());
}

#[test]
fn chk2() {
    // CHK2.L (A0),D0
    let bounds = [0, 0, 0, 1, 0, 0, 0, 9];
    let cpu = run(&[0x04D0, 0x0800], &bounds, |cpu| {
        cpu.regs.write_d::<Long>(0, 5);
    });
    assert_eq!(cpu.regs.pc, CODE + 4);

    let cpu = run(&[0x04D0, 0x0800], &bounds, |cpu| {
        cpu.regs.write_d::<Long>(0, 10);
    });
    assert_eq!(cpu.regs.pc, CHK_HANDLER);
    assert!(cpu.regs.sr.c());
    // The stacked PC is that of the next instruction
    assert_eq!(read_long(&cpu, cpu.regs.isp + 2), CODE + 4);
}

#[test]
fn data_register_mode_is_illegal() {
    // CMP2.B D1,D0
    let cpu = run(&[0x00C1, 0x0000], &[], |_| ());
    assert_eq!(cpu.regs.pc, ILLEGAL_HANDLER);
    // The stacked PC is that of the instruction
    assert_eq!(read_long(&cpu, cpu.regs.isp + 2), CODE);
}
