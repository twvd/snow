//! Six word exception stack frame (format $2) tests for 68020

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68020Fpu, M68020_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68020Fpu<Testbus<Address, u8, Bus68020>>;

const INITIAL_SSP: Address = 0x800;
const INITIAL_PC: Address = 0x1000;
const HANDLER_ADDR: Address = 0x2000;

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let hi = Word::from(*cpu.bus.mem.get(&addr).unwrap_or(&0));
    let lo = Word::from(*cpu.bus.mem.get(&(addr + 1)).unwrap_or(&0));
    (hi << 8) | lo
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    (Long::from(read_word(cpu, addr)) << 16) | Long::from(read_word(cpu, addr + 2))
}

/// Runs the instruction at INITIAL_PC + 4 (after a MOVE to CCR that sets
/// `ccr`) and checks it stacks a format $2 frame for `vector` that RTE pops
fn test_format2(code: &[u16], ccr: Word, vector: Address, next_pc: Address) {
    let mut cpu = TestCpu::new(Testbus::new(M68020_ADDRESS_MASK));
    write_word(&mut cpu, INITIAL_PC, 0x44FC); // MOVE #ccr,CCR
    write_word(&mut cpu, INITIAL_PC + 2, ccr);
    for (i, &word) in code.iter().enumerate() {
        write_word(&mut cpu, INITIAL_PC + 4 + i as Address * 2, word);
    }
    write_word(&mut cpu, HANDLER_ADDR, 0x4E73); // RTE
    write_word(&mut cpu, vector, (HANDLER_ADDR >> 16) as Word);
    write_word(&mut cpu, vector + 2, HANDLER_ADDR as Word);

    cpu.regs.isp = INITIAL_SSP;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.set_pc(INITIAL_PC).unwrap();
    cpu.prefetch_refill().unwrap();

    cpu.step().unwrap();
    let sr = cpu.regs.sr.sr();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, HANDLER_ADDR);
    assert_eq!(cpu.regs.isp, INITIAL_SSP - 12);
    // The instruction may change the condition codes before the exception
    let stacked_sr = read_word(&cpu, INITIAL_SSP - 12);
    assert_eq!(stacked_sr & 0xFF00, sr & 0xFF00);
    assert_eq!(read_long(&cpu, INITIAL_SSP - 10), next_pc);
    assert_eq!(read_word(&cpu, INITIAL_SSP - 6), 0x2000 | vector as Word);
    assert_eq!(read_long(&cpu, INITIAL_SSP - 4), INITIAL_PC + 4);

    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, next_pc);
    assert_eq!(cpu.regs.isp, INITIAL_SSP);
    assert_eq!(cpu.regs.sr.sr(), stacked_sr);
}

#[test]
fn test_trapv() {
    // TRAPV with V set
    test_format2(&[0x4E76, 0x4E71, 0x4E71], 0x02, 0x1C, INITIAL_PC + 6);
}

#[test]
fn test_trapcc_w() {
    // TRAPT.W #$1234
    test_format2(
        &[0x50FA, 0x1234, 0x4E71, 0x4E71],
        0x00,
        0x1C,
        INITIAL_PC + 8,
    );
}

#[test]
fn test_chk() {
    // CHK.W #-1,D0 with D0 = 0, above the bound
    test_format2(
        &[0x41BC, 0xFFFF, 0x4E71, 0x4E71],
        0x00,
        0x18,
        INITIAL_PC + 8,
    );
}

#[test]
fn test_divide_by_zero() {
    // DIVU.W #0,D0
    test_format2(
        &[0x80FC, 0x0000, 0x4E71, 0x4E71],
        0x00,
        0x14,
        INITIAL_PC + 8,
    );
}
