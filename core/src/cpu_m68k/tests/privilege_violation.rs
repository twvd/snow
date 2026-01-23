//! Privilege violation exception tests
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

/// Test privilege violation exception for a given privileged instruction
fn test_privilege_violation_exception(code: &[u16], test_name: &str) {
    const INITIAL_SSP: Address = 0x100;
    const INITIAL_PC: Address = 0x100; // Dangerously low, but OK for this test!

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install exception handler
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e73, // RTE
        0xffff, // sentinel
    ];

    const HANDLER_ADDR: Address = 0x200; // Dangerously low, but OK for this test!
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set privilege violation exception vector (vector 8, at address 0x20)
    write_long(&mut cpu, 0x20, HANDLER_ADDR);

    // Verify first program word was fetched (prefetch is already filled by testcpu)
    assert_eq!(
        cpu.prefetch.get(0).copied().unwrap_or(0),
        code[0],
        "{}: First program word was not fetched",
        test_name
    );

    // Put the CPU in user mode (not supervisor mode) so privileged instruction triggers a privilege violation
    assert!(
        cpu.regs.sr.supervisor(),
        "{}: Expect CPU to be in supervisor mode initially.",
        test_name
    );

    // Execute the first instruction, which should put the CPU into user mode i.e. set S to zero
    cpu.step().unwrap();
    assert!(
        !cpu.regs.sr.supervisor(),
        "{}: Expect first instruction to put CPU into user mode.",
        test_name
    );

    // Now execute the privileged code to trigger a privilege violation exception.
    let cycles_start = cpu.cycles;
    let initial_sr = cpu.regs.sr.sr();
    let initial_c = cpu.regs.sr.c();
    let initial_v = cpu.regs.sr.v();
    let initial_z = cpu.regs.sr.z();
    let initial_n = cpu.regs.sr.n();
    let initial_x = cpu.regs.sr.x();
    let violating_instruction_address = cpu.regs.pc;

    // Execute the privileged instruction - should trigger privilege violation, not execute
    cpu.step().unwrap();

    assert!(
        cpu.regs.sr.supervisor(),
        "{}: Expect CPU to be in supervisor mode following privilege violation exception.",
        test_name
    );

    // PC should have prefetched 2 handler words and point to last word fetched.
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

    // Condition codes should not be affected by the exception
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
        stack_frame_pc, violating_instruction_address,
        "{}: Privilege violation exception stack frame PC should be address of fault instruction.",
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
        "{}: Expect privilege violation to take 34 clocks",
        test_name
    );

    // Step the CPU again to execute the RTE in the handler.
    cpu.step().unwrap();

    // Check that the CPU is back on the privileged instruction again
    assert_eq!(
        cpu.regs.pc, violating_instruction_address,
        "{}: Expect PC to be back on the privileged instruction after RTE.",
        test_name
    );

    // Prefetch cache should have been refilled following RTE
    assert_eq!(
        cpu.prefetch.get(0).copied().unwrap_or(0),
        code[2],
        "{}: Prefetch cache[0] has not been refilled following RTE.",
        test_name
    );
    assert_eq!(
        cpu.prefetch.get(1).copied().unwrap_or(0),
        code[3],
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
fn test_ori_to_sr() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x007C, 0x0000, // ORI #$0000,SR     ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "ORI to SR");
}

#[test]
fn test_andi_to_sr() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x027C, 0x0000, // ANDI #$0000,SR    ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "ANDI to SR");
}

#[test]
fn test_eori_to_sr() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x0A7C, 0x0000, // EORI #$0000,SR    ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "EORI to SR");
}

#[test]
fn test_move_to_sr() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "MOVE to SR");
}

#[test]
fn test_move_to_usp() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x4E60,         // MOVE A0,USP       ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "MOVE to USP");
}

#[test]
fn test_move_from_usp() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x4E68,         // MOVE USP,A0       ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "MOVE from USP");
}

#[test]
fn test_reset() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x4E70,         // RESET             ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "RESET");
}

#[test]
fn test_stop() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x4E72, 0x1234, // STOP #$1234       ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "STOP");
}

#[test]
fn test_rte() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x46FC, 0x0000, // MOVE.W #$0000,SR  ; disable supervisor mode
        0x4E73,         // RTE               ; privilege violation
        0x4e71, 0x4e71, // NOP, NOP
    ];
    test_privilege_violation_exception(code, "RTE");
}
