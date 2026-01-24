//! Illegal instruction exception tests
//!
//! Ported with permission from tests written by Howard Price

use crate::bus::testbus::Testbus;
use crate::bus::Address;
use crate::cpu_m68k::instruction::Instruction;
use crate::cpu_m68k::{CpuM68000, M68000, M68000_ADDRESS_MASK};
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

/// Write a long (big-endian) to memory
fn write_long(cpu: &mut TestCpu, addr: Address, value: Long) {
    let addr = addr & M68000_ADDRESS_MASK;
    cpu.bus.mem.insert(addr, (value >> 24) as u8);
    cpu.bus.mem.insert(addr + 1, (value >> 16) as u8);
    cpu.bus.mem.insert(addr + 2, (value >> 8) as u8);
    cpu.bus.mem.insert(addr + 3, value as u8);
}

/// Read a word (big-endian) from memory
fn read_word(cpu: &TestCpu, addr: Address) -> Word {
    let addr = addr & M68000_ADDRESS_MASK;
    let hi = *cpu.bus.mem.get(&addr).unwrap_or(&0) as Word;
    let lo = *cpu.bus.mem.get(&(addr + 1)).unwrap_or(&0) as Word;
    (hi << 8) | lo
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

/// Test illegal instruction exception
fn test_illegal_exception(code: &[u16], exception_vector: Address, test_name: &str) {
    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
        0xffff, // sentinel
    ];

    const HANDLER_ADDR: Address = 0x400;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set exception vector
    write_long(&mut cpu, exception_vector, HANDLER_ADDR);

    // Execute the illegal instruction
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    let initial_c = cpu.regs.sr.c();
    let initial_v = cpu.regs.sr.v();
    let initial_z = cpu.regs.sr.z();
    let initial_n = cpu.regs.sr.n();
    let initial_x = cpu.regs.sr.x();

    cpu.step().unwrap();

    // Should be in handler
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "{}: Expect PC to point to handler",
        test_name
    );

    // First two handler words should have been prefetched
    assert_eq!(
        cpu.prefetch.get(0).copied().unwrap_or(0),
        handler_code[0],
        "{}: First handler word not prefetched",
        test_name
    );
    assert_eq!(
        cpu.prefetch.get(1).copied().unwrap_or(0),
        handler_code[1],
        "{}: Second handler word not prefetched",
        test_name
    );

    assert!(
        cpu.regs.sr.supervisor(),
        "{}: Expect CPU to be in supervisor mode following illegal instruction exception.",
        test_name
    );

    // Condition codes should not be affected by illegal instruction
    assert_eq!(
        cpu.regs.sr.c(),
        initial_c,
        "{}: Carry flag should not be affected",
        test_name
    );
    assert_eq!(
        cpu.regs.sr.v(),
        initial_v,
        "{}: Overflow flag should not be affected",
        test_name
    );
    assert_eq!(
        cpu.regs.sr.z(),
        initial_z,
        "{}: Zero flag should not be affected",
        test_name
    );
    assert_eq!(
        cpu.regs.sr.n(),
        initial_n,
        "{}: Negative flag should not be affected",
        test_name
    );
    assert_eq!(
        cpu.regs.sr.x(),
        initial_x,
        "{}: Extend flag should not be affected",
        test_name
    );

    // Check exception stack frame
    assert_eq!(
        cpu.regs.isp,
        INITIAL_SSP - 6,
        "{}: Group 1 and 2 stack frame size incorrect",
        test_name
    );

    let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
    assert_eq!(
        stack_frame_pc, INITIAL_PC,
        "{}: Illegal instruction exception stack frame PC should be address of illegal instruction.",
        test_name
    );

    let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
    assert_eq!(
        stack_frame_sr, initial_sr,
        "{}: Stack frame SR mismatch",
        test_name
    );

    let length_cycles = cpu.cycles - cycles_start;
    assert_eq!(
        length_cycles, 34,
        "{}: Expect illegal instruction exception to take 34 clocks",
        test_name
    );

    // Step the CPU again to execute the RTE in the handler
    cpu.step().unwrap();

    // Check that the CPU is back on the illegal instruction
    assert_eq!(
        cpu.regs.pc, INITIAL_PC,
        "{}: Expect PC to be back on the illegal instruction after RTE.",
        test_name
    );

    // Prefetch cache should have been refilled following RTE
    assert_eq!(
        cpu.prefetch.get(0).copied().unwrap_or(0),
        code[0],
        "{}: Expect to be back on original illegal instruction after RTE.",
        test_name
    );
    assert_eq!(
        cpu.prefetch.get(1).copied().unwrap_or(0),
        code[1],
        "{}: Prefetch cache[1] has not been refilled following RTE.",
        test_name
    );

    assert_eq!(
        cpu.regs.sr.sr(),
        initial_sr,
        "{}: Expect SR to be restored from stack after RTE.",
        test_name
    );

    assert_eq!(
        cpu.regs.isp, INITIAL_SSP,
        "{}: Expect stack pointer to be back to initial SSP after RTE.",
        test_name
    );
}

#[test]
fn test_official_illegal_0x4afc() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4afc, // official ILLEGAL opcode
        0x4e71, // NOP
    ];
    test_illegal_exception(code, 0x10, "ILLEGAL 0x4AFC");
}

#[test]
fn test_official_illegal_0x4afa() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4afa, // illegal opcode reserved for Motorola
        0x4e71, // NOP
    ];
    test_illegal_exception(code, 0x10, "ILLEGAL 0x4AFA");
}

#[test]
fn test_official_illegal_0x4afb() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4afb, // illegal opcode reserved for Motorola
        0x4e71, // NOP
    ];
    test_illegal_exception(code, 0x10, "ILLEGAL 0x4AFB");
}

#[test]
fn test_illegal_exception_stack_frame_tamper() {
    // Demonstrates a trick used by Rob Northen's Copylock (Amiga)
    // The ILLEGAL instruction triggers the illegal exception handler.
    // The exception stack frame contains the address of the illegal instruction.
    // This address is modified in memory, so that the RTE returns to the
    // instruction following the ILLEGAL instruction.

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    #[rustfmt::skip]
    let code: &[u16] = &[
        // Initialise SSP
        0x4ff8, 0x1000, // 1000: LEA $1000,A7

        // Install illegal exception handler
        0x41FA, 0x000A, // 1004: LEA IllegalExceptionHandler(PC),A0
        0x21c8, 0x0010, // 1008: MOVE.L A0,$10

        // .loop:
        0x4afc,         // 100C: ILLEGAL
        0x60fc,         // 100E: BRA .loop

        // IllegalExceptionHandler:
        // The address of the illegal instruction is pushed in the exception stack frame.
        // We want to continue execution at the next instruction, so increment by size of
        // ILLEGAL instruction: 2 bytes. Top of stack is 2 byte SR, followed by 4 byte PC at offset 2.
        0x54af, 0x0002, // 1010: ADDQ.L #2,2(A7)
        0x4e73,         // 1014: RTE
    ];

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Step to ILLEGAL instruction
    while cpu.regs.pc != 0x100c {
        cpu.step().unwrap();
    }

    // Execute illegal instruction
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, 0x1010, "Should be in handler");

    // Step twice to patch exception stack frame and exit handler
    cpu.step().unwrap(); // ADDQ.L #2,2(A7)
    cpu.step().unwrap(); // RTE

    assert_eq!(
        cpu.regs.pc, 0x100e,
        "Should be at instruction following ILLEGAL"
    );
}

#[test]
fn test_other_illegal_opcodes() {
    // Test a sample of other illegal opcodes
    let sample_illegal_opcodes = [
        0x0000, // First possible illegal
        0x003C, // ORI to SR with invalid size
        0x01FF, // BTST with invalid mode
        0x4800, // Invalid NBCD mode
        0x4900, // Invalid EXT
        0x49C0, // Invalid EXT
        0x4AC0, // Invalid TAS mode
        0x4E74, // RTD (68010+, illegal on 68000)
        0x4E75, // RTS (valid, but let's test decode works)
        0x4E76, // TRAPV (valid)
        0x4E77, // RTR (valid)
    ];

    for &opcode in &sample_illegal_opcodes {
        // Check if this opcode is illegal for 68000
        if Instruction::try_decode(M68000, opcode).is_err() {
            let code: &[u16] = &[opcode, 0x4e71];
            test_illegal_exception(code, 0x10, &format!("ILLEGAL opcode 0x{:04X}", opcode));
        }
    }
}

#[test]
fn test_line_a_exceptions() {
    // Test a sample of Line A opcodes (0xA000-0xAFFF)
    let sample_opcodes = [
        0xA000, // First Line A
        0xA123, // Random Line A
        0xA800, // Mid Line A
        0xAABC, // Random Line A
        0xAFFF, // Last Line A
    ];

    for &opcode in &sample_opcodes {
        let code: &[u16] = &[opcode, 0x4e71];
        test_illegal_exception(code, 0x28, &format!("LINE A 0x{:04X}", opcode));
    }
}

#[test]
fn test_line_f_exceptions() {
    // Test a sample of Line F opcodes (0xF000-0xFFFF)
    let sample_opcodes = [
        0xF000, // First Line F
        0xF123, // Random Line F
        0xF800, // Mid Line F
        0xFABC, // Random Line F
        0xFFFE, // Near last Line F
    ];

    for &opcode in &sample_opcodes {
        let code: &[u16] = &[opcode, 0x4e71];
        test_illegal_exception(code, 0x2C, &format!("LINE F 0x{:04X}", opcode));
    }
}

#[test]
fn test_all_line_a_opcodes() {
    // Test all Line A opcodes (0xA000-0xAFFF)
    for opcode in 0xA000..=0xAFFF {
        let code: &[u16] = &[opcode, 0x4e71];
        test_illegal_exception(code, 0x28, &format!("LINE A 0x{:04X}", opcode));
    }
}

#[test]
fn test_all_line_f_opcodes() {
    // Test all Line F opcodes (0xF000-0xFFFF)
    for opcode in 0xF000..=0xFFFF {
        let code: &[u16] = &[opcode, 0x4e71];
        test_illegal_exception(code, 0x2C, &format!("LINE F 0x{:04X}", opcode));
    }
}

#[test]
fn test_all_illegal_opcodes() {
    // Test all opcodes that decode to ILLEGAL for 68000
    for opcode in 0u16..=0xFFFF {
        if Instruction::try_decode(M68000, opcode).is_err() {
            let code: &[u16] = &[opcode, 0x4e71];
            test_illegal_exception(code, 0x10, &format!("ILLEGAL 0x{:04X}", opcode));
        }
    }
}
