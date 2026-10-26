//! PACK and UNPK (68020 and later)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::{Byte, Long, Word};

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const SRC: Address = 0x3000;
const DST: Address = 0x4000;

/// Runs `opcode` with `adjustment`, A0 = SRC and A1 = DST
fn run(opcode: Word, adjustment: Word, setup: impl Fn(&mut TestCpu)) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68020_ADDRESS_MASK));
    for (i, w) in [opcode, adjustment, 0x4E71, 0x4E71].into_iter().enumerate() {
        let addr = CODE + 2 * i as Address;
        cpu.bus.mem.insert(addr, (w >> 8) as u8);
        cpu.bus.mem.insert(addr + 1, w as u8);
    }
    cpu.regs.isp = 0x800;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.write_a(0, SRC);
    cpu.regs.write_a(1, DST);
    setup(&mut cpu);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 4);
    cpu
}

fn mem(cpu: &TestCpu, addr: Address) -> Byte {
    cpu.bus.mem[&addr]
}

#[test]
fn pack_registers() {
    // PACK D0,D1,#0
    let cpu = run(0x8340, 0, |cpu| {
        cpu.regs.write_d::<Long>(0, 0x0304);
        cpu.regs.write_d::<Long>(1, 0xAABB_CCDD);
    });
    assert_eq!(cpu.regs.read_d::<Long>(1), 0xAABB_CC34);

    // ASCII "12" to packed BCD
    let cpu = run(0x8340, 0x3030u16.wrapping_neg(), |cpu| {
        cpu.regs.write_d::<Long>(0, 0x3132)
    });
    assert_eq!(cpu.regs.read_d::<Byte>(1), 0x12);
}

#[test]
fn pack_memory() {
    // PACK -(A0),-(A1),#0
    let cpu = run(0x8348, 0, |cpu| {
        cpu.bus.mem.insert(SRC - 2, 0x05);
        cpu.bus.mem.insert(SRC - 1, 0x09);
    });
    assert_eq!(mem(&cpu, DST - 1), 0x59);
    assert_eq!(cpu.regs.read_a::<Address>(0), SRC - 2);
    assert_eq!(cpu.regs.read_a::<Address>(1), DST - 1);

    // PACK -(A0),-(A7),#0: a byte moves A7 by 2
    let cpu = run(0x8F48, 0, |cpu| {
        cpu.bus.mem.insert(SRC - 2, 0x01);
        cpu.bus.mem.insert(SRC - 1, 0x02);
    });
    assert_eq!(cpu.regs.isp, 0x800 - 2);
    assert_eq!(mem(&cpu, 0x800 - 2), 0x12);
}

#[test]
fn unpk_registers() {
    // UNPK D0,D1,#$3030
    let cpu = run(0x8380, 0x3030, |cpu| {
        cpu.regs.write_d::<Long>(0, 0xFF47);
        cpu.regs.write_d::<Long>(1, 0xAABB_CCDD);
    });
    assert_eq!(cpu.regs.read_d::<Long>(1), 0xAABB_3437);
}

#[test]
fn unpk_memory() {
    // UNPK -(A0),-(A1),#0
    let cpu = run(0x8388, 0, |cpu| {
        cpu.bus.mem.insert(SRC - 1, 0x82);
    });
    assert_eq!(mem(&cpu, DST - 2), 0x08);
    assert_eq!(mem(&cpu, DST - 1), 0x02);
    assert_eq!(cpu.regs.read_a::<Address>(0), SRC - 1);
    assert_eq!(cpu.regs.read_a::<Address>(1), DST - 2);
}
