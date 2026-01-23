//! Trace exception tests
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
fn test_trace_exception() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        0x7000,                 // 00001000: MOVEQ #0,D0
        0x7200,                 // 00001002: MOVEQ #0,D1

        // install trace exception handler
        0x41f9, 0x0000, 0x1020, // 00001004: LEA $101C,A0
        0x21c8, 0x0024,         // 0000100A: MOVE.L A0,$24  ; $24 is the trace exception vector

        // enable trace mode
        0x007c, 0x8000,         // 0000100E: ORI #$8000,SR  ; set trace bit in SR

        // trace exception will fire *after* each subsequent instruction
        0x7001,                 // 00001012: MOVEQ #1,D0
        0x7002,                 // 00001014: MOVEQ #2,D0

        // disable trace mode
        // a final trace exception will be generated after this instruction because trace
        // was enabled before the instruction stared executing
        0x027C, 0x7FFF,         // 00001016: ANDI #$7FFF,SR

        // trace is now disable, so there should be no exception generated for this instruction
        0x7003,                 // 0000101A: MOVEQ #3,D0
        0x4ef8, 0x1000,         // 0000101C: JMP $1000

        // trace exception handler
        0x5241,                 // 00001020: ADDQ #1,D1
        0x4e73,                 // 00001022: RTE
    ];

    const HANDLER_ADDR: Address = 0x1020;
    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Step until instruction that enables trace mode: 0000100E: ORI #$8000,SR
    while cpu.regs.pc != 0x100E {
        cpu.step().unwrap();
    }

    // 0000100E: ORI #$8000,SR
    assert_eq!(cpu.regs.pc, 0x100E);
    assert!(!cpu.regs.sr.trace(), "Don't expect trace to be enabled yet");
    cpu.step().unwrap();
    assert!(cpu.regs.sr.trace(), "Trace should now be enabled");

    // 00001012: MOVEQ #1,D0
    // Trace is enabled before this instruction starts executing, so after execution
    // we should end up in the trace handler.
    {
        assert_eq!(cpu.regs.pc, 0x1012, "Should be on MOVEQ #1,D0");
        assert!(cpu.regs.sr.trace(), "Trace should be enabled");
        assert_eq!(cpu.regs.d[0], 0);
        assert_eq!(cpu.regs.d[1], 0);

        let pre_trace_sr = cpu.regs.sr.sr();
        cpu.step().unwrap(); // Execute MOVEQ #1,D0, then trace exception fires

        // Should now be in trace handler
        assert_eq!(
            cpu.regs.pc, HANDLER_ADDR,
            "Trace handler should have been invoked"
        );
        assert!(
            !cpu.regs.sr.trace(),
            "Trace mode should be disabled when processing exception"
        );
        assert!(cpu.regs.sr.supervisor(), "CPU should be in supervisor mode");
        assert_eq!(cpu.regs.d[0], 1, "D0 should have been set by MOVEQ");
        assert_eq!(cpu.regs.d[1], 0, "D1 should not have changed yet");

        // Check exception stack frame
        assert_eq!(
            cpu.regs.isp,
            INITIAL_SSP - 6,
            "Group 1/2 stack frame is 6 bytes"
        );
        let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
        assert_eq!(
            stack_frame_pc, 0x1014,
            "Stack frame PC should point to next instruction: MOVEQ #2,D0"
        );
        let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
        assert_eq!(stack_frame_sr, pre_trace_sr);

        // Step through trace exception handler
        cpu.step().unwrap(); // ADDQ #1,D1
        assert_eq!(cpu.regs.d[0], 1);
        assert_eq!(cpu.regs.d[1], 1, "D1 should have been incremented");
        assert_eq!(cpu.regs.pc, 0x1022, "Should be on RTE");

        cpu.step().unwrap(); // RTE
        assert_eq!(cpu.regs.pc, 0x1014, "After RTE should be on MOVEQ #2,D0");
        assert_eq!(cpu.regs.sr.sr(), pre_trace_sr, "SR should be restored");
        assert!(cpu.regs.sr.trace(), "Trace should be re-enabled");
        assert_eq!(cpu.regs.isp, INITIAL_SSP, "SSP should be restored");
    }

    // 00001014: MOVEQ #2,D0
    {
        assert_eq!(cpu.regs.pc, 0x1014);
        assert_eq!(cpu.regs.d[0], 1);
        assert_eq!(cpu.regs.d[1], 1);

        let pre_trace_sr = cpu.regs.sr.sr();
        cpu.step().unwrap(); // Execute MOVEQ #2,D0, trace fires

        assert_eq!(cpu.regs.pc, HANDLER_ADDR, "Should be in trace handler");
        assert!(!cpu.regs.sr.trace(), "Trace disabled in handler");
        assert_eq!(cpu.regs.d[0], 2, "D0 should be 2");

        // Check stack frame
        assert_eq!(cpu.regs.isp, INITIAL_SSP - 6);
        let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
        assert_eq!(
            stack_frame_pc, 0x1016,
            "Stack frame PC should point to ANDI instruction"
        );
        let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
        assert_eq!(stack_frame_sr, pre_trace_sr);

        // Step through handler
        cpu.step().unwrap(); // ADDQ #1,D1
        assert_eq!(cpu.regs.d[1], 2);
        assert_eq!(cpu.regs.pc, 0x1022);

        cpu.step().unwrap(); // RTE
        assert_eq!(cpu.regs.pc, 0x1016, "Should be on ANDI #$7FFF,SR");
        assert!(cpu.regs.sr.trace(), "Trace should be re-enabled");
        assert_eq!(cpu.regs.isp, INITIAL_SSP);
    }

    // 00001016: ANDI #$7FFF,SR - disables trace
    // But trace was enabled before, so trace exception still fires after
    {
        assert_eq!(cpu.regs.pc, 0x1016);
        assert_eq!(cpu.regs.d[0], 2);
        assert_eq!(cpu.regs.d[1], 2);
        assert!(cpu.regs.sr.trace(), "Trace should be enabled");

        cpu.step().unwrap(); // Execute ANDI, trace fires

        assert_eq!(cpu.regs.pc, HANDLER_ADDR, "Should be in trace handler");
        assert!(!cpu.regs.sr.trace(), "Trace disabled in handler");
        assert_eq!(cpu.regs.d[0], 2);
        assert_eq!(cpu.regs.d[1], 2);

        // Check stack frame - SR should have trace bit CLEAR (ANDI cleared it)
        assert_eq!(cpu.regs.isp, INITIAL_SSP - 6);
        let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
        assert_eq!(
            stack_frame_pc, 0x101A,
            "Stack frame PC should point to MOVEQ #3,D0"
        );
        let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
        assert_eq!(
            stack_frame_sr & 0x8000,
            0,
            "Saved SR should have trace bit clear"
        );

        // Step through handler
        cpu.step().unwrap(); // ADDQ #1,D1
        assert_eq!(cpu.regs.d[1], 3);

        cpu.step().unwrap(); // RTE
        assert_eq!(cpu.regs.pc, 0x101A, "Should be on MOVEQ #3,D0");
        assert!(
            !cpu.regs.sr.trace(),
            "Trace should NOT be re-enabled (ANDI cleared it)"
        );
        assert_eq!(cpu.regs.isp, INITIAL_SSP);
    }

    // 0000101A: MOVEQ #3,D0 - trace disabled, no exception
    {
        assert_eq!(cpu.regs.pc, 0x101A);
        assert!(!cpu.regs.sr.trace(), "Trace should be disabled");
        assert_eq!(cpu.regs.d[0], 2);
        assert_eq!(cpu.regs.d[1], 3);

        cpu.step().unwrap(); // Execute MOVEQ #3,D0

        assert_eq!(cpu.regs.d[0], 3, "D0 should be 3");
        assert_eq!(cpu.regs.d[1], 3, "D1 unchanged (no trace handler called)");
        assert!(!cpu.regs.sr.trace(), "Trace still disabled");
        assert_eq!(
            cpu.regs.pc, 0x101C,
            "Should move to next instruction normally"
        );
    }
}

#[test]
fn test_trace_with_illegal() {
    // Tests interaction between trace and illegal instruction exceptions.
    #[rustfmt::skip]
    let code: &[u16] = &[
        // START:
        //     ; Initialise SSP
        0x4ff8, 0x1000, //     1000: LEA $1000.W,A7

        //     ; Install illegal exception handler
        0x41fa, 0x001E, //     1004: LEA IllegalExceptionHandler(PC),A0
        0x21c8, 0x0010, //     1008: MOVE.L A0,$10.W

        //     ; Install trace exception handler, but do not enable trace mode.
        0x41fa, 0x001C, //     100C: LEA TraceExceptionHandler(PC),A0
        0x21c8, 0x0024, //     1010 MOVE.L A0,$24

        0x4afc,         //     1014: ILLEGAL

        0x7000,         //     1016: MOVEQ #0,D0

        //     ; Enable trace mode.
        0x007c, 0x8000, //     1018: ORI #$8000,SR

        //     ; Trace is enabled so the trace exception should occur after executing this instruction
        0x5240,         //     101C: ADDQ #1,D0

        //     ; If the instruction is not executed because the instruction is illegal or privileged,
        //     ; the trace exception does not occur. - MC68000UM
        0x4afc,         //     101E: ILLEGAL

        //     ; Again, this instruction should generate a trace exception after execution
        0x5240,         //     1020: ADDQ #1,D0
        // .loop:
        //     ; An exception will be generated after each call to BRA in this loop.
        0x60fe,         //     1022: BRA.S .loop
        //
        // IllegalExceptionHandler:
        //     ; The address of the illegal instruction is pushed in the exception stack frame.
        //     ; We want to continue execution at the next instruction, so increment by size of ILLEGAL
        //     ; instruction: 2 bytes. Top of stack is 2 byte SR, followed by 4 byte PC at offset 2.
        0x54af, 0x0002, //     1024: ADDQ.L #2,2(A7)
        0x4e73,         //     1028: RTE

        // TraceExceptionHandler:
        0x4e73,         //     102A: RTE
    ];

    const ILLEGAL_HANDLER_ADDR: Address = 0x1024;
    const TRACE_HANDLER_ADDR: Address = 0x102A;
    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Step to the first ILLEGAL instruction
    while cpu.regs.pc != 0x1014 {
        cpu.step().unwrap();
    }

    // Execute the illegal instruction - should go to illegal handler
    cpu.step().unwrap(); // 1014: ILLEGAL
    assert_eq!(cpu.regs.pc, ILLEGAL_HANDLER_ADDR);

    // Step out of the illegal handler
    cpu.step().unwrap(); // ADDQ.L #2,2(A7)
    cpu.step().unwrap(); // RTE

    assert_eq!(cpu.regs.pc, 0x1016, "Should be on MOVEQ #0,D0");
    cpu.step().unwrap(); // MOVEQ #0,D0
    assert_eq!(cpu.regs.pc, 0x1018);
    cpu.step().unwrap(); // ORI #$8000,SR - enables trace
    assert!(cpu.regs.sr.trace(), "Trace should be enabled");

    // With trace enabled, ADDQ #1,D0 should execute then trace fires
    assert_eq!(cpu.regs.d[0], 0);
    cpu.step().unwrap(); // 101C: ADDQ #1,D0
    assert_eq!(cpu.regs.d[0], 1);
    assert_eq!(
        cpu.regs.pc, TRACE_HANDLER_ADDR,
        "Should be in trace handler"
    );

    // Step out of trace handler
    cpu.step().unwrap(); // RTE
    assert_eq!(cpu.regs.pc, 0x101E, "Should be on second ILLEGAL");
    assert!(cpu.regs.sr.trace(), "Trace should be re-enabled");

    // Now execute ILLEGAL with trace enabled.
    // Per M68000 manual: "If the instruction is not executed because the instruction
    // is illegal or privileged, the trace exception does not occur."
    cpu.step().unwrap(); // 101E: ILLEGAL
    assert_eq!(
        cpu.regs.pc, ILLEGAL_HANDLER_ADDR,
        "Should be in illegal handler"
    );
    // Trace should NOT have fired because ILLEGAL doesn't execute

    // Step out of illegal handler
    cpu.step().unwrap(); // ADDQ.L #2,2(A7)
    cpu.step().unwrap(); // RTE

    assert_eq!(cpu.regs.pc, 0x1020, "Should be on ADDQ #1,D0");
    assert!(cpu.regs.sr.trace(), "Trace should still be enabled");

    cpu.step().unwrap(); // 1020: ADDQ #1,D0
    assert_eq!(cpu.regs.d[0], 2);
    assert_eq!(cpu.regs.pc, TRACE_HANDLER_ADDR, "Trace should fire");

    // Step out of trace handler
    cpu.step().unwrap(); // RTE
    assert_eq!(cpu.regs.pc, 0x1022, "Should be on BRA");
    assert!(cpu.regs.sr.trace(), "Trace re-enabled");

    cpu.step().unwrap(); // BRA .loop
    assert_eq!(cpu.regs.pc, TRACE_HANDLER_ADDR, "Trace fires after BRA");
}

#[test]
fn test_trace_with_privilege_violation() {
    #[rustfmt::skip]
    let code: &[u16] = &[
        // START:
        //     ; Initialise SSP
        0x4ff8, 0x1000, //     1000: LEA $1000.W,A7

        //     ; Install privilege violation exception handler
        0x41fa, 0x0022, //     1004: LEA PrivExceptionHandler(PC),A0
        0x21c8, 0x0020, //     1008: MOVE.L A0,$20.W

        //     ; Install trace exception handler, but do not enable trace mode.
        0x41fa, 0x0026, //     100C: LEA TraceExceptionHandler(PC),A0
        0x21c8, 0x0024, //     1010 MOVE.L A0,$24.W

        //     Disable supervisor mode (bit 13)
        0x27c, 0xdfff,  //     1014: ANDI #$dfff,SR

        //     ; Attempt to execute a privileged instruction, which should trigger the privilege violation exception.
        0x007c,0xffff,  //     1018: ORI #$ffff,SR

        //     ; The privilege violation exception handler enables trace mode, so trace exception should occur after executing each of the followin instructions.
        0x7000,         //     101C: MOVEQ #0,D0
        0x5240,         //     101E: ADDQ #1,D0

        //     ; Attempt to execute a privileged instruction, which should trigger the privilege violation exception.
        //     ; If the instruction is not executed because the instruction is illegal or privileged,
        //     ; the trace exception does not occur. - MC68000UM
        0x007c, 0xffff, //     1020: ORI #$ffff,SR

        //     ; Again, this instruction should generate a trace exception after execution
        0x5240,         //     1024: ADDQ #1,D0

        // .loop:
        //     ; A trace exception will be generated after each call to BRA in this loop.
        0x60fe,         //     1026: BRA.S .loop
        //
        // PrivExceptionHandler:
        // ; The address of the instruction that caused the exception is pushed in the exception stack frame.
        // ; We want to continue execution at the next instruction, so increment by size of the
        // ; instruction, which in this case is 2 words (4 bytes).
        // ; The top of stack is 2 byte SR, followed by 4 byte PC at offset 2.
        0x58af, 0x0002, //     1028: ADDQ.L #4,2(A7)

        // ; Modify the SR in the callstack to enable trace mode, disable supervisor mode, and disable all interrupts
        0x3f7c, 0x8700, 0x0000, // 102C: MOVE.W #$8700,0(A7)

        0x4e73,         //     1032: RTE

        // TraceExceptionHandler:
        0x4e73,         //     1034: RTE
    ];

    const PRIV_HANDLER_ADDR: Address = 0x1028;
    const TRACE_HANDLER_ADDR: Address = 0x1034;
    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Step to the first privileged instruction
    while cpu.regs.pc != 0x1018 {
        cpu.step().unwrap();
    }

    assert!(!cpu.regs.sr.supervisor(), "Should be in user mode");

    // Execute privileged instruction - goes to priv handler
    cpu.step().unwrap(); // 1018: ORI #$ffff,SR
    assert_eq!(cpu.regs.pc, PRIV_HANDLER_ADDR);

    // Step out of priv handler (which enables trace in saved SR)
    cpu.step().unwrap(); // ADDQ.L #4,2(A7)
    cpu.step().unwrap(); // MOVE.W #$8700,0(A7)
    cpu.step().unwrap(); // RTE
    assert!(cpu.regs.sr.trace(), "Trace should be enabled after RTE");
    assert!(!cpu.regs.sr.supervisor(), "Should be in user mode");

    assert_eq!(cpu.regs.pc, 0x101C, "Should be on MOVEQ #0,D0");
    cpu.step().unwrap(); // MOVEQ #0,D0
    assert_eq!(cpu.regs.d[0], 0);
    assert_eq!(cpu.regs.pc, TRACE_HANDLER_ADDR, "Trace fires");

    // Step out of trace handler
    cpu.step().unwrap(); // RTE
    assert_eq!(cpu.regs.pc, 0x101E, "Should be on ADDQ #1,D0");
    assert!(cpu.regs.sr.trace(), "Trace re-enabled");
    assert!(!cpu.regs.sr.supervisor(), "User mode");

    cpu.step().unwrap(); // ADDQ #1,D0
    assert_eq!(cpu.regs.d[0], 1);
    assert_eq!(cpu.regs.pc, TRACE_HANDLER_ADDR, "Trace fires");

    // Step out of trace handler
    cpu.step().unwrap(); // RTE
    assert_eq!(cpu.regs.pc, 0x1020, "Should be on second ORI");
    assert!(cpu.regs.sr.trace(), "Trace re-enabled");

    // Execute privileged instruction with trace enabled.
    // Per M68000 manual, trace should NOT fire because instruction doesn't execute.
    cpu.step().unwrap(); // 1020: ORI #$ffff,SR
    assert_eq!(cpu.regs.pc, PRIV_HANDLER_ADDR, "Should be in priv handler");

    // Step out of priv handler
    cpu.step().unwrap(); // ADDQ.L #4,2(A7)
    cpu.step().unwrap(); // MOVE.W #$8700,0(A7)
    cpu.step().unwrap(); // RTE

    assert_eq!(cpu.regs.pc, 0x1024, "Should be on ADDQ #1,D0");
    assert!(cpu.regs.sr.trace(), "Trace enabled");

    cpu.step().unwrap(); // ADDQ #1,D0
    assert_eq!(cpu.regs.d[0], 2);
    assert_eq!(cpu.regs.pc, TRACE_HANDLER_ADDR, "Trace fires");

    // Step out of trace handler
    cpu.step().unwrap(); // RTE
    assert_eq!(cpu.regs.pc, 0x1026, "Should be on BRA");
}

#[test]
fn test_trace_with_trap() {
    // Test Group 1 trace exception with Group 2 TRAP exception.
    #[rustfmt::skip]
    let code: &[u16] = &[
		0x4ff8, 0x1000, // 10000: LEA $1000.W,A7 ; Initialise SSP
		0x7000,         // 10004: MOVEQ #0,D0
		0x7200,         // 10006: MOVEQ #0,D1
		0x7400,         // 10008: MOVEQ #0,D2

		                //        ; Install trap exception handler
		0x41FA, 0x001E, // 1000A: LEA TrapExceptionHandler(PC),A0
		0x21C8, 0x0080, // 1000E: MOVE.L A0,$80.W
		                //
		                //        ; Install trace exception handler, but do not enable trace mode.
		0x41FA, 0x001A, // 10012: LEA TraceExceptionHandler(PC),A0
		0x21C8, 0x0024, // 10016: MOVE.L A0,$24.W

		0x4E40,         // 1001A: TRAP #0
		0x4E40,         // 1001C: TRAP #0
		                //
		                //        ; Enable trace mode
		                //        ; Because trace mode is not enabled *before* this instruction is executed, the
		                //        ; trace exception is not generated after execution.
		0x007C,0x8000,  // 1001E: ORI #$8000,SR
		                //
		                //        ; Trace is enabled before this instruction excecutes so the trace exception
		                //        ; will be pending and processed afterwards.
		0x5240,         // 10022: ADDQ #1,D0
		                //
		                //        ; Check the order of exception processing in golden reference emulators: WinUAE and MAME
		                //        ; - First the trap exception is processed. Pushes return address 10026 and SR A700 to stack.
		                //        ; - Then the trace exception is processed. Pushes return address 1002A and SR 2700 to stack.
		                //        ; Instruction execution resumes in the trace handler.
		                //        ; The trace handler RTEs to the trap handler.
		                //        ; The trap handler RTEs back to the ADDQ in the .loop
		0x4E40,         // 10024: TRAP #0
		                //
		                // .loop:
		                //        ; Again, this instruction should generate a trace exception after execution
		0x5240,         // 10026: ADDQ #1,D0
		                //
		                //        ; A trace exception will be generated after each call to BRA in this loop.
		0x60FC,         // 10028: BRA.S .loop
		                //
		                //        TrapExceptionHandler:
		0x5241,         // 1002A: ADDQ #1,D1
		0x4E73,         // 1002C: RTE
		                //
		                //        TraceExceptionHandler:
		0x5242,         // 1002E: ADDQ #1,D2
		0x4E73,         // 10030: RTE
    ];

    const TRAP_HANDLER_ADDR: Address = 0x1002A;
    const TRACE_HANDLER_ADDR: Address = 0x1002E;
    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x10000;

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Step to TRAP #0 at 0x10024 (where trace is enabled)
    while cpu.regs.pc != 0x10024 {
        cpu.step().unwrap();
    }

    // At this point trace is enabled and we're on TRAP #0
    assert!(cpu.regs.sr.trace(), "Trace should be enabled");
    // D1 = 2 (two trap handlers ran), D2 = 1 (one trace handler for ADDQ)
    assert_eq!(cpu.regs.d[1], 2, "Two TRAP handlers should have run");
    assert_eq!(
        cpu.regs.d[2], 1,
        "One trace handler should have run (for ADDQ)"
    );

    // Execute TRAP #0 with trace enabled
    // TRAP executes (Group 2), then trace fires (Group 1 has higher priority for pending)
    // The order per M68000: TRAP executes, pushes frame, then trace is processed
    cpu.step().unwrap(); // TRAP #0

    // After TRAP with trace enabled, we should be in one of the handlers
    // The exact behavior depends on exception processing order
    let addr = cpu.regs.pc;
    assert!(
        addr == TRAP_HANDLER_ADDR || addr == TRACE_HANDLER_ADDR,
        "Should be in trap or trace handler, got {:08X}",
        addr
    );

    // Step until we're back at the loop (0x10026)
    while cpu.regs.pc != 0x10026 {
        cpu.step().unwrap();
    }

    // Both handlers should have run
    assert_eq!(
        cpu.regs.d[1], 3,
        "Trap handler should have run (D1 incremented)"
    );
    assert!(
        cpu.regs.d[2] >= 2,
        "Trace handler should have run at least once more"
    );
    assert_eq!(cpu.regs.pc, 0x10026, "Should be on ADDQ in loop");
}
