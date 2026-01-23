//! Group 2 exception tests
//!
//! Ported with permission from tests written by Howard Price

use crate::bus::testbus::Testbus;
use crate::bus::Address;
use crate::cpu_m68k::{CpuM68000, M68000_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestCpu = CpuM68000<Testbus<Address, u8>>;

/// Initialize a test system with code at a given PC
fn testcpu(initial_ssp: Address, initial_pc: Address, code: &[u16]) -> TestCpu {
    let bus = Testbus::new(M68000_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);

    // Write code to memory
    for (i, &word) in code.iter().enumerate() {
        let addr = initial_pc + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set up CPU state
    cpu.regs.isp = initial_ssp;
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);

    // Use set_pc to properly initialize PC and clear prefetch,
    // then fill prefetch from memory
    cpu.set_pc(initial_pc).expect("set_pc failed");
    cpu.prefetch_refill().expect("prefetch_refill failed");

    cpu
}

/// Write a word (big-endian) to memory
fn write_word(cpu: &mut TestCpu, addr: Address, value: Word) {
    let addr = addr & M68000_ADDRESS_MASK;
    cpu.bus.mem.insert(addr, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 1, value as u8);
}

/// Read a word (big-endian) from memory
fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let addr = addr & M68000_ADDRESS_MASK;
    let hi = *cpu.bus.mem.get(&addr).unwrap_or(&0) as Word;
    let lo = *cpu.bus.mem.get(&(addr + 1)).unwrap_or(&0) as Word;
    (hi << 8) | lo
}

/// Write a long (big-endian) to memory
fn write_long(cpu: &mut TestCpu, addr: Address, value: Long) {
    let addr = addr & M68000_ADDRESS_MASK;
    cpu.bus.mem.insert(addr, (value >> 24) as u8);
    cpu.bus.mem.insert(addr + 1, (value >> 16) as u8);
    cpu.bus.mem.insert(addr + 2, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 3, value as u8);
}

/// Read a long (big-endian) from memory
fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    let addr = addr & M68000_ADDRESS_MASK;
    let b0 = *cpu.bus.mem.get(&addr).unwrap_or(&0) as Long;
    let b1 = *cpu.bus.mem.get(&(addr + 1)).unwrap_or(&0) as Long;
    let b2 = *cpu.bus.mem.get(&(addr + 2)).unwrap_or(&0) as Long;
    let b3 = *cpu.bus.mem.get(&(addr + 3)).unwrap_or(&0) as Long;
    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
}

#[test]
fn test_chk() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x7000,         // 1000: MOVEQ #0,D0
        0x72FF,         // 1002: MOVEQ #-1,D1
        0x4380,         // 1004: CHK D0,D1  If D1 < 0 or D1 > D0, then a CHK exception occurs
        0x4ef8, 0x1000, // 1006: JMP $1000
    ];

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install CHK exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
    ];

    const HANDLER_ADDR: Address = 0x600;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set CHK exception vector (vector 6, at address 0x18)
    write_long(&mut cpu, 0x18, HANDLER_ADDR);

    // Step to the CHK instruction
    cpu.step().unwrap(); // MOVEQ #0,D0
    cpu.step().unwrap(); // MOVEQ #-1,D1

    // Execute the CHK instruction, which should cause a CHK exception
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    cpu.step().unwrap(); // CHK D0,D1

    // CHK exception should be generated and processed within the step
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "Expect CHK instruction address to be the next instruction after CHK."
    );

    // Check the exception stack frame
    assert_eq!(
        cpu.regs.isp,
        INITIAL_SSP - 6,
        "Group 2 stack frame size incorrect"
    );

    let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
    assert_eq!(
        stack_frame_pc, 0x1006,
        "CHK exception stack frame PC should be address of instruction following CHK."
    );

    let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
    assert_eq!(stack_frame_sr, initial_sr);

    assert_eq!(
        cpu.cycles - cycles_start,
        40,
        "Expect CHK duration to be 40 clocks."
    );
}

/// Test TRAP instruction with a specific trap index
fn test_trap_with_index(trap_index: u16) {
    assert!(trap_index <= 15, "TRAP index must be in range [0,15]");

    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4E40 | trap_index, // 1000: TRAP #n
        0x4ef8, 0x1000,      // 1002: JMP $1000
    ];

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install TRAP exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
    ];

    const HANDLER_ADDR: Address = 0x600;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set TRAP exception vector (TRAP #0 is vector 32 at address 0x80, +4 for each TRAP)
    let vector_addr = 0x80 + (trap_index as u32 * 4);
    write_long(&mut cpu, vector_addr, HANDLER_ADDR);

    // Execute the TRAP instruction, which will generate a TRAP exception
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    cpu.step().unwrap(); // TRAP #n

    // TRAP exception should be generated and processed within the step
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "Expect TRAP instruction address to be the next instruction after TRAP."
    );

    // Check the exception stack frame
    assert_eq!(
        cpu.regs.isp,
        INITIAL_SSP - 6,
        "Group 2 stack frame size incorrect"
    );

    let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
    assert_eq!(
        stack_frame_pc, 0x1002,
        "TRAP exception stack frame PC should be address of instruction following TRAP."
    );

    let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
    assert_eq!(stack_frame_sr, initial_sr);

    assert_eq!(
        cpu.cycles - cycles_start,
        34,
        "Expect TRAP duration to be 34 clocks."
    );
}

#[test]
fn test_trap() {
    for i in 0..=15 {
        test_trap_with_index(i);
    }
}

#[test]
fn test_trapv() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x303C, 0x7fff, // 1000: MOVE.W #$7FFF,D0 ; INT16_MAX
        0x5240,         // 1004: ADDQ.W #1,D0    ; $8000 = -1, so MSb has changed indicating overflow
        0x4e76,         // 1006: TRAPV
        0x4ef8, 0x1000, // 1008: JMP $1000
    ];

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install TRAPV exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
    ];

    const HANDLER_ADDR: Address = 0x600;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set TRAPV exception vector (vector 7, at address 0x1C)
    write_long(&mut cpu, 0x1C, HANDLER_ADDR);

    // Step to the TRAPV instruction
    cpu.step().unwrap(); // MOVE.W #$7FFF,D0
    cpu.step().unwrap(); // ADDQ.W #1,D0

    assert!(
        cpu.regs.sr.v(),
        "Expect overflow flag to be set after ADDQ.W #1,D0."
    );

    // Execute the TRAPV instruction, which should cause a TRAPV exception
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    cpu.step().unwrap(); // TRAPV

    // TRAPV exception should be generated and processed within the step
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "Expect TRAPV instruction address to be the next instruction after TRAPV."
    );

    // Check the exception stack frame
    assert_eq!(
        cpu.regs.isp,
        INITIAL_SSP - 6,
        "Group 2 stack frame size incorrect"
    );

    let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
    assert_eq!(
        stack_frame_pc, 0x1008,
        "TRAPV exception stack frame PC should be address of instruction following TRAPV."
    );

    let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
    assert_eq!(stack_frame_sr, initial_sr);

    assert_eq!(
        cpu.cycles - cycles_start,
        34,
        "Expect TRAPV exception processing time to be 34 clocks."
    );
}

#[test]
fn test_div0_divu() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x7000, // 1000: MOVEQ #$0,D0
        0x7201, // 1002: MOVEQ #$1,D1
        0x82C0, // 1004: DIVU D0,D1 ; Divides the unsigned destination operand by the unsigned source operand and stores the unsigned result in the destination.
        0x60f8, // 1006: BRA $1000
    ];

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install DIV0 exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
    ];

    const HANDLER_ADDR: Address = 0x600;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set ZeroDivide exception vector (vector 5, at address 0x14)
    write_long(&mut cpu, 0x14, HANDLER_ADDR);

    // Step to the DIVU instruction
    cpu.step().unwrap(); // MOVEQ #$0,D0
    cpu.step().unwrap(); // MOVEQ #$1,D1

    // Execute the DIVU instruction, which should cause a ZeroDivide exception
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    cpu.step().unwrap(); // DIVU D0,D1

    // DIV0 exception should be generated and processed within the step
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "Expect instruction address to be the next instruction after DIV."
    );

    // Check the exception stack frame
    assert_eq!(
        cpu.regs.isp,
        INITIAL_SSP - 6,
        "Group 2 stack frame size incorrect"
    );

    let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
    assert_eq!(
        stack_frame_pc, 0x1006,
        "DIV0 exception stack frame PC should be address of instruction following DIV instruction."
    );

    let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
    assert_eq!(stack_frame_sr, initial_sr);

    assert_eq!(
        cpu.cycles - cycles_start,
        38,
        "Expect DIV0 exception processing time to be 38 clocks."
    );
}

#[test]
fn test_div0_divs() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x7000, // 1000: MOVEQ #$0,D0
        0x7201, // 1002: MOVEQ #$1,D1
        0x83C0, // 1004: DIVS D0,D1 ; Divides the signed destination operand by the signed source operand and stores the signed result in the destination.
        0x60f8, // 1006: BRA $1000
    ];

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install DIV0 exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
    ];

    const HANDLER_ADDR: Address = 0x600;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set ZeroDivide exception vector (vector 5, at address 0x14)
    write_long(&mut cpu, 0x14, HANDLER_ADDR);

    // Step to the DIVS instruction
    cpu.step().unwrap(); // MOVEQ #$0,D0
    cpu.step().unwrap(); // MOVEQ #$1,D1

    // Execute the DIVS instruction, which should cause a ZeroDivide exception
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    cpu.step().unwrap(); // DIVS D0,D1

    // DIV0 exception should be generated and processed within the step
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "Expect instruction address to be the next instruction after DIV."
    );

    // Check the exception stack frame
    assert_eq!(
        cpu.regs.isp,
        INITIAL_SSP - 6,
        "Group 2 stack frame size incorrect"
    );

    let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
    assert_eq!(
        stack_frame_pc, 0x1006,
        "DIV0 exception stack frame PC should be address of instruction following DIV instruction."
    );

    let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
    assert_eq!(stack_frame_sr, initial_sr);

    assert_eq!(
        cpu.cycles - cycles_start,
        38,
        "Expect DIV0 exception processing time to be 38 clocks."
    );
}
