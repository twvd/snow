//! 68030 bus error frames and RTE: a handler completes the faulted data
//! cycle by clearing DF (M68030UM 8.2)

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68030Fpu<Testbus<Address, u8, Bus68020>>;

const CODE: Address = 0x1000;
const HANDLER: Address = 0x2000;
const STACK: Address = 0x8000;
const FAULT: Address = 0x3000;

fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

fn write_words(cpu: &mut TestCpu, addr: Address, words: &[Word]) {
    for (i, &w) in words.iter().enumerate() {
        write_word(cpu, addr + 2 * i as Address, w);
    }
}

fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let byte = |a: Address| Word::from(*cpu.bus.mem.get(&a).unwrap_or(&0));
    (byte(addr) << 8) | byte(addr + 1)
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    (Long::from(read_word(cpu, addr)) << 16) | Long::from(read_word(cpu, addr + 2))
}

/// Sets up `code` with the bus error handler `handler` and bus errors on
/// FAULT..FAULT+3
fn setup(code: &[Word], handler: &[Word], init: impl Fn(&mut TestCpu)) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68030_ADDRESS_MASK));
    write_words(&mut cpu, CODE, code);
    write_words(&mut cpu, HANDLER, handler);
    write_word(&mut cpu, 8, (HANDLER >> 16) as Word);
    write_word(&mut cpu, 10, HANDLER as Word);
    cpu.bus.fault_ranges.push(FAULT..=FAULT + 3);
    cpu.regs.isp = STACK;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    init(&mut cpu);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu
}

fn step(cpu: &mut TestCpu, steps: usize) {
    for _ in 0..steps {
        cpu.step().unwrap();
    }
}

/// Checks the bus error frame just stacked: format, SSW and fault address
fn check_frame(cpu: &TestCpu, format: Word, ssw: Word, address: Address) {
    let sp = *cpu.regs.ssp();
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(read_word(cpu, sp + 6), format << 12 | 8);
    assert_eq!(read_word(cpu, sp + 0x0A), ssw);
    assert_eq!(read_long(cpu, sp + 0x10), address);
}

/// ANDI.W #$FEFF,($A,A7): clears DF
const CLEAR_DF: [Word; 3] = [0x026F, 0xFEFF, 0x000A];
/// RTE
const RTE: Word = 0x4E73;
/// NOP
const NOP: Word = 0x4E71;

#[test]
fn read_completed_from_data_input_buffer() {
    // MOVE.L (A0),D0 ; NOP
    // Handler: clear DF ; MOVE.L #$CAFEBABE,($2C,A7) ; RTE
    let mut handler = CLEAR_DF.to_vec();
    handler.extend([0x2F7C, 0xCAFE, 0xBABE, 0x002C, RTE]);
    let mut cpu = setup(&[0x2010, NOP], &handler, |cpu| {
        cpu.regs.write_a::<Address>(0, FAULT);
    });

    step(&mut cpu, 1);
    // A read fault at an instruction boundary still stacks the long frame:
    // DF, read, long, supervisor data
    check_frame(&cpu, 0xB, 0x0145, FAULT);

    // Handler, replay
    step(&mut cpu, 4);
    assert_eq!(cpu.regs.pc, CODE + 2);
    assert_eq!(cpu.regs.read_d::<Long>(0), 0xCAFE_BABE);
    step(&mut cpu, 1);
    assert_eq!(cpu.regs.pc, CODE + 4);
    assert_eq!(*cpu.regs.ssp(), STACK);
}

#[test]
fn read_rerun_with_df_set() {
    // MOVE.L (A0),D0 ; NOP
    // Handler: RTE
    let mut cpu = setup(&[0x2010, NOP], &[RTE], |cpu| {
        cpu.regs.write_a::<Address>(0, FAULT);
    });
    step(&mut cpu, 1);
    check_frame(&cpu, 0xB, 0x0145, FAULT);

    // The fault is gone: RTE reruns the read
    cpu.bus.fault_ranges.clear();
    for (i, b) in [0x12, 0x34, 0x56, 0x78].into_iter().enumerate() {
        cpu.bus.mem.insert(FAULT + i as Address, b);
    }
    step(&mut cpu, 2);
    assert_eq!(cpu.regs.pc, CODE + 2);
    assert_eq!(cpu.regs.read_d::<Long>(0), 0x1234_5678);
}

#[test]
fn read_completed_in_second_cycle() {
    // MOVE.L (A0),D0 with A0 = FAULT - 2: the first cycle reads the two
    // bytes below FAULT, the second faults with two bytes left
    // Handler: clear DF ; MOVE.L #$0000BEEF,($2C,A7) ; RTE
    let mut handler = CLEAR_DF.to_vec();
    handler.extend([0x2F7C, 0x0000, 0xBEEF, 0x002C, RTE]);
    let mut cpu = setup(&[0x2010, NOP], &handler, |cpu| {
        cpu.regs.write_a::<Address>(0, FAULT - 2);
        cpu.bus.mem.insert(FAULT - 2, 0xDE);
        cpu.bus.mem.insert(FAULT - 1, 0xAD);
    });

    step(&mut cpu, 1);
    // DF, read, word, supervisor data
    check_frame(&cpu, 0xB, 0x0165, FAULT);

    step(&mut cpu, 4);
    assert_eq!(cpu.regs.pc, CODE + 2);
    assert_eq!(cpu.regs.read_d::<Long>(0), 0xDEAD_BEEF);
}

#[test]
fn write_completed_by_handler() {
    // MOVE.L D0,(A0) ; NOP
    // Handler: MOVE.L ($18,A7),D7 ; clear DF ; RTE
    let mut handler = vec![0x2E2F, 0x0018];
    handler.extend(CLEAR_DF);
    handler.push(RTE);
    let mut cpu = setup(&[0x2080, NOP], &handler, |cpu| {
        cpu.regs.write_a::<Address>(0, FAULT);
        cpu.regs.write_d::<Long>(0, 0x1234_5678);
    });

    step(&mut cpu, 1);
    assert_eq!(cpu.regs.pc, HANDLER);
    let sp = *cpu.regs.ssp();
    // DF, write, long, supervisor data
    assert_eq!(read_word(&cpu, sp + 0x0A), 0x0105);
    assert_eq!(read_long(&cpu, sp + 0x10), FAULT);

    // Handler, replay: the write does not go to the bus again
    step(&mut cpu, 4);
    assert_eq!(cpu.regs.pc, CODE + 2);
    assert_eq!(cpu.regs.read_d::<Long>(7), 0x1234_5678);
    assert_eq!(*cpu.regs.ssp(), STACK);
    step(&mut cpu, 1);
    assert_eq!(cpu.regs.pc, CODE + 4);
}
