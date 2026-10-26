//! Exception stack frames of the 68020 and later: format $2, RTE

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
fn chk2() {
    // CHK2.L (A0),D0 with D0 above the bounds 1 and 9
    let cpu = run(&[0x04D0, 0x0800], 1, |cpu| {
        cpu.regs.write_a::<Address>(0, 0x3000);
        for (i, b) in [0, 0, 0, 1, 0, 0, 0, 9].into_iter().enumerate() {
            cpu.bus.mem.insert(0x3000 + i as Address, b);
        }
        cpu.regs.write_d::<Long>(0, 10);
    });
    check_frame(&cpu, CODE + 4, 0x18, CODE);
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

#[test]
fn rte() {
    // TRAPV, then RTE in the handler
    let mut cpu = run(&[0x4E76, 0x4E71], 1, |cpu| {
        cpu.regs.sr.set_v(true);
        write_word(cpu, HANDLER, 0x4E73);
    });
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 2);
    assert_eq!(*cpu.regs.ssp(), STACK);
}

#[test]
fn rte_unknown_format_is_format_error() {
    // RTE with a format $5 frame
    let cpu = run(&[0x4E73], 1, |cpu| {
        for (i, w) in [0x2700, 0x0000, 0x3000, 0x5000].iter().enumerate() {
            write_word(cpu, STACK - 8 + 2 * i as Address, *w);
        }
        cpu.regs.isp = STACK - 8;
    });
    // Format error, the frame is left in place and the PC is the RTE
    let sp = *cpu.regs.ssp();
    assert_eq!(sp, STACK - 16);
    assert_eq!(read_long(&cpu, sp + 2), CODE);
    assert_eq!(read_word(&cpu, sp + 6), 0x0038);
}

#[test]
fn rte_throwaway_frame() {
    // RTE on the interrupt stack with a throwaway frame whose SR selects the
    // master stack, holding a normal frame
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    write_word(&mut cpu, CODE, 0x4E73);
    for (i, w) in [0x3700, 0x0000, 0x0000, 0x1000].iter().enumerate() {
        write_word(&mut cpu, STACK - 8 + 2 * i as Address, *w);
    }
    for (i, w) in [0x2000, 0x0000, 0x3000, 0x0000].iter().enumerate() {
        write_word(&mut cpu, 0x9000 - 8 + 2 * i as Address, *w);
    }
    cpu.regs.isp = STACK - 8;
    cpu.regs.msp = 0x9000 - 8;
    cpu.regs.sr.set_supervisor(true);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();

    assert_eq!(cpu.regs.pc, 0x3000);
    assert_eq!(cpu.regs.isp, STACK);
    assert_eq!(cpu.regs.msp, 0x9000);
    assert!(!cpu.regs.sr.m());
}

#[test]
fn frestore_unknown_frame_is_format_error() {
    // FRESTORE (A0)+ of a frame that is not of the 68882
    let cpu = run(&[0xF358], 1, |cpu| {
        write_word(cpu, 0x4000, 0x1234);
        cpu.regs.write_a(0, 0x4000_u32);
    });
    assert_eq!(cpu.regs.read_a::<Address>(0), 0x4000);
    let sp = *cpu.regs.ssp();
    assert_eq!(sp, STACK - 8);
    assert_eq!(read_long(&cpu, sp + 2), CODE);
    assert_eq!(read_word(&cpu, sp + 6), 0x0038);
}
