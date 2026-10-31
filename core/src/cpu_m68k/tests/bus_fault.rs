//! 68000 bus errors: the frame a fault handler restarts an instruction from

use crate::bus::Address;
use crate::bus::access::Bus68000;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68000, FPU_NONE, M68000_ADDRESS_MASK};
use crate::types::Word;

type TestCpu = CpuM68000<Testbus<Address, u8, Bus68000>>;

const CODE: Address = 0x1000;
const SSP: Address = 0x8000;
const HANDLER: Address = 0x4000;
const ABSENT: Address = 0x20_0000;
const FC_USER_DATA: Word = 1;
const FC_USER_PROGRAM: Word = 2;
const SSW_READ: Word = 1 << 4;

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let hi = *cpu.bus.mem.get(&addr).unwrap_or(&0) as Word;
    let lo = *cpu.bus.mem.get(&(addr + 1)).unwrap_or(&0) as Word;
    (hi << 8) | lo
}

/// A CPU in user mode, with memory at `ABSENT` and up ending in a bus error
fn testcpu(code: &[Word]) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68000_ADDRESS_MASK), FPU_NONE);
    cpu.bus.fault_ranges.push(ABSENT..=(ABSENT + 0xFFFF));
    write_word(&mut cpu, 0x0008, (HANDLER >> 16) as Word);
    write_word(&mut cpu, 0x000A, HANDLER as Word);
    for (i, &word) in code.iter().enumerate() {
        write_word(&mut cpu, CODE + i as Address * 2, word);
    }
    // NOPs at the handler
    for i in 0..4 {
        write_word(&mut cpu, HANDLER + i * 2, 0x4E71);
    }
    cpu.regs.isp = SSP;
    cpu.regs.usp = 0x6000;
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.regs.sr.set_supervisor(false);
    cpu
}

struct Frame {
    ssw: Word,
    address: Address,
    ir: Word,
    pc: Address,
}

/// Runs one instruction, which has to take the bus error
fn fault_frame(cpu: &mut TestCpu) -> Frame {
    cpu.step().unwrap();
    assert!(cpu.regs.sr.supervisor());
    assert_eq!(cpu.regs.isp, SSP - 14);
    let frame = SSP - 14;
    let long = |cpu: &TestCpu, a| (read_word(cpu, a) as u32) << 16 | read_word(cpu, a + 2) as u32;
    Frame {
        ssw: read_word(cpu, frame),
        address: long(cpu, frame + 2),
        ir: read_word(cpu, frame + 6),
        pc: long(cpu, frame + 10),
    }
}

/// The instruction after the faulting one still comes from the handler
fn assert_in_handler(cpu: &mut TestCpu) {
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER + 2);
}

#[test]
fn jmp_long_into_absent_segment() {
    let (target_hi, target_lo) = ((ABSENT >> 16) as Word, ABSENT as Word);
    let mut cpu = testcpu(&[0x4EF9, target_hi, target_lo]);
    let frame = fault_frame(&mut cpu);
    assert_eq!(frame.ir, 0x4EF9);
    assert_eq!(frame.pc, CODE + 2);
    assert_eq!(frame.address, ABSENT);
    assert_eq!(frame.ssw & 0x1F, SSW_READ | FC_USER_PROGRAM);
    assert_in_handler(&mut cpu);
}

#[test]
fn jsr_long_into_absent_segment() {
    let (target_hi, target_lo) = ((ABSENT >> 16) as Word, ABSENT as Word);
    let mut cpu = testcpu(&[0x4EB9, target_hi, target_lo]);
    let frame = fault_frame(&mut cpu);
    assert_eq!(frame.ir, 0x4EB9);
    assert_eq!(frame.pc, CODE + 6);
    assert_eq!(frame.address, ABSENT);
}

#[test]
fn rts_into_absent_segment() {
    let mut cpu = testcpu(&[0x4E75]);
    write_word(&mut cpu, 0x6000, (ABSENT >> 16) as Word);
    write_word(&mut cpu, 0x6002, ABSENT as Word);
    let frame = fault_frame(&mut cpu);
    assert_eq!(frame.ir, 0x4E75);
    assert_eq!(frame.pc, CODE + 2);
    assert_eq!(frame.address, ABSENT);
}

/// The stack probe: UniSoft's `backup()` and the Lisa OS read the word at PC - 2
#[test]
fn stack_probe() {
    // TST.B d16(A7)
    let mut cpu = testcpu(&[0x4A2F, 0x0010]);
    cpu.regs.usp = ABSENT;
    let frame = fault_frame(&mut cpu);
    assert_eq!(frame.ir, 0x4A2F);
    assert_eq!(frame.pc, CODE + 2);
    assert_eq!(read_word(&cpu, frame.pc - 2), 0x4A2F);
    assert_eq!(frame.address, ABSENT + 0x10);
    assert_eq!(frame.ssw & 0x1F, SSW_READ | FC_USER_DATA);
    assert_in_handler(&mut cpu);
}

#[test]
fn data_write() {
    // MOVE.W D0,(A0)
    let mut cpu = testcpu(&[0x3080]);
    cpu.regs.a[0] = ABSENT;
    let frame = fault_frame(&mut cpu);
    assert_eq!(frame.ssw & 0x1F, FC_USER_DATA);
    assert_eq!(frame.address, ABSENT);
    assert_in_handler(&mut cpu);
}
