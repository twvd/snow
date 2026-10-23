//! Overlap of the FPU arithmetic operations with the CPU, 68020 + 68881

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::Word;

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

const INITIAL_PC: Address = 0x1000;
const HANDLER_ADDR: Address = 0x2000;

#[rustfmt::skip]
const CODE: &[Word] = &[
    0xF200, 0x0422, // FADD.X FP1,FP0
    0x7001,         // MOVEQ #1,D0
    0xF200, 0x0422, // FADD.X FP1,FP0
    0x4E71, 0x4E71, // NOP, NOP
];

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

fn testcpu() -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68020_ADDRESS_MASK));
    for (i, &word) in CODE.iter().enumerate() {
        write_word(&mut cpu, INITIAL_PC + i as Address * 2, word);
    }
    write_word(&mut cpu, HANDLER_ADDR, 0x4E71);
    // Level 3 autovector
    write_word(&mut cpu, 0x6C, (HANDLER_ADDR >> 16) as Word);
    write_word(&mut cpu, 0x6E, HANDLER_ADDR as Word);

    cpu.regs.isp = 0x800;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(0);
    cpu.set_pc(INITIAL_PC).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

#[test]
fn test_overlap() {
    let mut cpu = testcpu();

    // FADD FPn: 51 clocks on the 68881, of which the CPU waits 3, and the
    // prefetch refill (a 3 clock bus cycle) already overlaps
    cpu.step().unwrap();
    let busy = cpu.fpu_free_at - cpu.cycles;
    assert_eq!(busy, 51 - 3 - 3);

    // The MOVEQ runs while the FPU is busy
    let start = cpu.cycles;
    cpu.step().unwrap();
    assert!(cpu.cycles - start < busy);

    // The next FADD waits for the first one
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, INITIAL_PC + 10);
    assert!(cpu.cycles >= start + busy);
}

#[test]
fn test_interrupt_while_busy() {
    let mut cpu = testcpu();
    cpu.step().unwrap();
    cpu.step().unwrap();

    // An interrupt while the FPU is busy is taken before the second FADD,
    // which starts again after it
    cpu.bus.irq_level = Some(3);
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER_ADDR);
    let stacked_pc = (Address::from(cpu.bus.mem[&(0x800 - 6)]) << 24)
        | (Address::from(cpu.bus.mem[&(0x800 - 5)]) << 16)
        | (Address::from(cpu.bus.mem[&(0x800 - 4)]) << 8)
        | Address::from(cpu.bus.mem[&(0x800 - 3)]);
    assert_eq!(stacked_pc, INITIAL_PC + 6);
}
