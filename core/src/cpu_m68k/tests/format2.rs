//! Six word (format $2) exception stack frames of the 68020 and later

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68030Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const HANDLER: Address = 0x2000;
const STACK: Address = 0x8000;

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let byte = |a: Address| Word::from(*cpu.bus.mem.get(&a).unwrap_or(&0));
    (byte(addr) << 8) | byte(addr + 1)
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    (Long::from(read_word(cpu, addr)) << 16) | Long::from(read_word(cpu, addr + 2))
}

/// Runs `steps` instructions of `code` with every vector pointing at HANDLER
fn run(code: &[Word], steps: usize, setup: impl Fn(&mut TestCpu)) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    for (i, &w) in code.iter().enumerate() {
        write_word(&mut cpu, CODE + 2 * i as Address, w);
    }
    for vector in 2..64 {
        write_word(&mut cpu, vector * 4, (HANDLER >> 16) as Word);
        write_word(&mut cpu, vector * 4 + 2, HANDLER as Word);
    }
    cpu.regs.isp = STACK;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    setup(&mut cpu);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    for _ in 0..steps {
        cpu.step().unwrap();
    }
    assert_eq!(cpu.regs.pc, HANDLER);
    cpu
}

/// Checks the frame: next PC, format and vector, instruction address
fn check_frame(cpu: &TestCpu, pc: Address, vector: Address, address: Address) {
    let sp = *cpu.regs.ssp();
    assert_eq!(sp, STACK - 12);
    assert_eq!(read_long(cpu, sp + 2), pc);
    assert_eq!(read_word(cpu, sp + 6), 0x2000 | vector as Word);
    assert_eq!(read_long(cpu, sp + 8), address);
}

#[test]
fn chk() {
    // MOVEQ #-1,D1 ; CHK.W D0,D1
    let cpu = run(&[0x72FF, 0x4380], 2, |_| ());
    check_frame(&cpu, CODE + 4, 0x18, CODE + 2);
}

#[test]
fn zero_divide() {
    // DIVU.W D0,D1
    let cpu = run(&[0x82C0], 1, |_| ());
    check_frame(&cpu, CODE + 2, 0x14, CODE);
}

#[test]
fn trapv() {
    // TRAPV
    let cpu = run(&[0x4E76], 1, |cpu| cpu.regs.sr.set_v(true));
    check_frame(&cpu, CODE + 2, 0x1C, CODE);
}

#[test]
fn trace() {
    // NOP
    let cpu = run(&[0x4E71], 1, |cpu| cpu.regs.sr.set_trace(true));
    check_frame(&cpu, CODE + 2, 0x24, CODE);
}
