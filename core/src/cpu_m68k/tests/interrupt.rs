//! Interrupt exception tests
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

/// Test single interrupt handling
fn test_single_interrupt(interrupt_priority_mask: u8, interrupt_pending_level: u8) {
    // Determine if interrupt should fire
    let expected_interrupt_level = if interrupt_pending_level == 7 {
        // Non-maskable interrupt
        7
    } else if interrupt_pending_level > interrupt_priority_mask {
        interrupt_pending_level
    } else {
        0
    };

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
    ];

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Load interrupt handler code
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

    if expected_interrupt_level > 0 {
        // Install interrupt handler
        // Autovector base is 0x64, each level adds 4 bytes
        let exception_vector = 0x64 + ((expected_interrupt_level as u32 - 1) * 4);
        write_long(&mut cpu, exception_vector, HANDLER_ADDR);
    }

    // Set interrupt priority mask
    cpu.regs.sr.set_int_prio_mask(interrupt_priority_mask);

    // Capture state before interrupt processing
    assert_eq!(
        cpu.prefetch.get(0).copied().unwrap_or(0),
        code[0],
        "Prefetch cache not initialised"
    );
    assert_eq!(
        cpu.prefetch.get(1).copied().unwrap_or(0),
        code[1],
        "Prefetch cache not initialised"
    );

    let pre_interrupt_cycles = cpu.cycles;
    let pre_interrupt_sr = cpu.regs.sr.sr();
    let pre_interrupt_c = cpu.regs.sr.c();
    let pre_interrupt_v = cpu.regs.sr.v();
    let pre_interrupt_z = cpu.regs.sr.z();
    let pre_interrupt_n = cpu.regs.sr.n();
    let pre_interrupt_x = cpu.regs.sr.x();

    // The PC that will be saved on stack (after NOP executes)
    let expected_stack_pc = INITIAL_PC + 2;

    // Set interrupt pending level and step the CPU.
    // In Snow, the instruction executes and then the interrupt is processed in the same step.
    cpu.bus.irq_level = Some(interrupt_pending_level);
    cpu.step().unwrap();
    cpu.bus.irq_level = None;

    if expected_interrupt_level > 0 {
        // Interrupt should have been processed - now in handler
        assert_eq!(
            cpu.regs.pc, HANDLER_ADDR,
            "Expect PC to be in interrupt handler (mask={}, level={})",
            interrupt_priority_mask, interrupt_pending_level
        );

        // First two handler words should have been prefetched
        assert_eq!(
            cpu.prefetch.get(0).copied().unwrap_or(0),
            handler_code[0],
            "First handler word not prefetched"
        );
        assert_eq!(
            cpu.prefetch.get(1).copied().unwrap_or(0),
            handler_code[1],
            "Second handler word not prefetched"
        );

        assert!(
            cpu.regs.sr.supervisor(),
            "Expect CPU to be in supervisor mode"
        );

        // Condition codes should not be affected by the interrupt
        assert_eq!(
            cpu.regs.sr.c(),
            pre_interrupt_c,
            "Carry flag should not be affected"
        );
        assert_eq!(
            cpu.regs.sr.v(),
            pre_interrupt_v,
            "Overflow flag should not be affected"
        );
        assert_eq!(
            cpu.regs.sr.z(),
            pre_interrupt_z,
            "Zero flag should not be affected"
        );
        assert_eq!(
            cpu.regs.sr.n(),
            pre_interrupt_n,
            "Negative flag should not be affected"
        );
        assert_eq!(
            cpu.regs.sr.x(),
            pre_interrupt_x,
            "Extend flag should not be affected"
        );

        // Check exception stack frame
        assert_eq!(
            cpu.regs.isp,
            INITIAL_SSP - 6,
            "Group 1 and 2 stack frame size incorrect"
        );

        let stack_frame_pc = read_long(&cpu, INITIAL_SSP - 4);
        assert_eq!(
            stack_frame_pc, expected_stack_pc,
            "Stack frame PC should be address of instruction that would have been executed"
        );

        let stack_frame_sr = read_word(&cpu, INITIAL_SSP - 6);
        assert_eq!(stack_frame_sr, pre_interrupt_sr, "Stack frame SR mismatch");

        // In Snow, interrupt processing happens at the end of the same step() that executes
        // the instruction. So total cycles = NOP execution (4 cycles) + interrupt (44 cycles).
        // We verify interrupt processing takes 44 cycles by subtracting the NOP overhead.
        let total_cycles = cpu.cycles - pre_interrupt_cycles;
        let nop_cycles = 4;
        let interrupt_cycles = total_cycles - nop_cycles;
        assert_eq!(
            interrupt_cycles, 44,
            "Expect interrupt processing to take 44 cycles (mask={}, level={})",
            interrupt_priority_mask, interrupt_pending_level
        );

        // Step the CPU again to execute the RTE in the handler
        cpu.step().unwrap();

        // Check that the CPU is back at the instruction after the one that executed
        assert_eq!(
            cpu.regs.pc, expected_stack_pc,
            "Expect PC to be back at saved PC after RTE"
        );

        // Prefetch cache should have been refilled following RTE
        assert_eq!(
            cpu.prefetch.get(0).copied().unwrap_or(0),
            code[1],
            "Prefetch cache[0] not refilled following RTE"
        );
        assert_eq!(
            cpu.prefetch.get(1).copied().unwrap_or(0),
            code[2],
            "Prefetch cache[1] not refilled following RTE"
        );

        assert_eq!(
            cpu.regs.sr.sr(),
            pre_interrupt_sr,
            "Expect SR to be restored from stack after RTE"
        );

        assert_eq!(
            cpu.regs.isp, INITIAL_SSP,
            "Expect stack pointer to be back to initial SSP after RTE"
        );
    } else {
        // Interrupt not expected - CPU should have just executed the NOP
        assert_eq!(
            cpu.regs.pc,
            INITIAL_PC + 2,
            "CPU should advance to next instruction when no interrupt (mask={}, level={})",
            interrupt_priority_mask,
            interrupt_pending_level
        );
    }
}

/// Test multiple interrupts (interrupt nesting)
fn test_multiple_interrupts(
    interrupt_priority_mask: u8,
    interrupt_pending_level1: u8,
    interrupt_pending_level2: u8,
) {
    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
    ];

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install an interrupt handler for each level
    for i in 1..=7 {
        // Load interrupt handler code
        #[rustfmt::skip]
        let handler_code: &[u16] = &[
            0x4e71, // NOP - start with a NOP so a step inside the handler won't exit immediately
            0x4e73, // RTE
        ];

        let handler_addr = 0x400 + (i * 0x10);
        for (j, &word) in handler_code.iter().enumerate() {
            let addr = handler_addr + (j as u32 * 2);
            write_word(&mut cpu, addr, word);
        }

        // Install interrupt handler
        let exception_vector = 0x64 + ((i as u32 - 1) * 4);
        write_long(&mut cpu, exception_vector, handler_addr);
    }

    // Set interrupt priority mask
    cpu.regs.sr.set_int_prio_mask(interrupt_priority_mask);

    assert_eq!(
        cpu.prefetch.get(0).copied().unwrap_or(0),
        code[0],
        "Prefetch cache not initialised"
    );
    assert_eq!(
        cpu.prefetch.get(1).copied().unwrap_or(0),
        code[1],
        "Prefetch cache not initialised"
    );

    // Step 1: Execute first instruction with IRQ1 set
    cpu.bus.irq_level = Some(interrupt_pending_level1);
    cpu.step().unwrap();

    // Step 2: Execute next instruction (either in handler or main program) with IRQ2 set
    // This tests interrupt masking and edge-triggered behavior for level 7:
    // - If level2 == level1, no interrupt (no level change, even for level 7)
    // - If level2 > level1 and level2 > mask, interrupt fires
    // - If level2 == 7 and level1 != 7, interrupt fires (rising edge to level 7)
    cpu.bus.irq_level = Some(interrupt_pending_level2);
    cpu.step().unwrap();

    // Step 3: Execute next instruction with no interrupts
    cpu.bus.irq_level = None;
    cpu.step().unwrap();

    // Now check that the PC is where we expect it to be.
    // - Step 1 executes instruction + processes IRQ1 (if applicable)
    // - Step 2 executes next instruction + processes IRQ2 (if applicable)
    // - Step 3 executes next instruction

    let first_interrupt_generated =
        (interrupt_pending_level1 > interrupt_priority_mask) || interrupt_pending_level1 == 7;

    // Level 7 fires on transition from lower level to 7 (edge-triggered).
    // If level2 == level1 == 7, there's no transition, so no second interrupt.
    // "An interrupt is generated each time the interrupt request level changes from
    // some lower level to level 7." - M68000 UM
    let second_interrupt_generated = ((interrupt_pending_level2 > interrupt_pending_level1)
        && (interrupt_pending_level2 > interrupt_priority_mask))
        || ((interrupt_pending_level1 != 7) && (interrupt_pending_level2 == 7)); // NMI doesn't interrupt itself

    if !first_interrupt_generated && !second_interrupt_generated {
        // 1. No interrupts: executed 3 NOPs from main program
        assert_eq!(
            cpu.regs.pc,
            INITIAL_PC + 6,
            "No interrupts: PC should be at 4th instruction (mask={}, level1={}, level2={})",
            interrupt_priority_mask,
            interrupt_pending_level1,
            interrupt_pending_level2
        );
    } else if first_interrupt_generated && !second_interrupt_generated {
        // 2. First interrupt only:
        // Step 1: NOP + IRQ1 → handler1
        // Step 2: NOP in handler
        // Step 3: RTE → back to INITIAL_PC + 2
        assert_eq!(
            cpu.regs.pc,
            INITIAL_PC + 2,
            "First interrupt only: RTE should return to main program (mask={}, level1={}, level2={})",
            interrupt_priority_mask,
            interrupt_pending_level1,
            interrupt_pending_level2
        );
    } else if !first_interrupt_generated && second_interrupt_generated {
        // 3. Second interrupt only:
        // Step 1: NOP → 0x1002
        // Step 2: NOP + IRQ2 → handler2
        // Step 3: NOP in handler → handler2 + 2
        let handler_addr = 0x400 + (interrupt_pending_level2 as u32 * 0x10);
        assert_eq!(
            cpu.regs.pc,
            handler_addr + 2,
            "Second interrupt only: PC should be at 2nd handler instruction (mask={}, level1={}, level2={})",
            interrupt_priority_mask,
            interrupt_pending_level1,
            interrupt_pending_level2
        );
    } else {
        // 4. Both interrupts:
        // Step 1: NOP + IRQ1 → handler1
        // Step 2: NOP in handler1 + IRQ2 → handler2
        // Step 3: NOP in handler2 → handler2 + 2
        let handler_addr = 0x400 + (interrupt_pending_level2 as u32 * 0x10);
        assert_eq!(
            cpu.regs.pc,
            handler_addr + 2,
            "Both interrupts: PC should be at 2nd handler instruction (mask={}, level1={}, level2={})",
            interrupt_priority_mask,
            interrupt_pending_level1,
            interrupt_pending_level2
        );
    }
}

#[test]
fn test_nmi_edge_triggered() {
    // Test that NMI (level 7) is edge-triggered, not level-sensitive.
    // "An interrupt is generated each time the interrupt request level changes from
    // some lower level to level 7." - M68000 UM
    //
    // This means:
    // - Holding level 7 continuously should NOT re-trigger
    // - Dropping to lower level then back to 7 SHOULD trigger again

    const INITIAL_SSP: Address = 0x1000;
    const INITIAL_PC: Address = 0x1000;

    #[rustfmt::skip]
    let code: &[u16] = &[
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
        0x4e71, // NOP
    ];

    let mut cpu = testcpu(INITIAL_SSP, INITIAL_PC, code);

    // Install NMI handler that just does NOP then RTE
    #[rustfmt::skip]
    let handler_code: &[u16] = &[
        0x4e71, // NOP
        0x4e73, // RTE
    ];

    const HANDLER_ADDR: Address = 0x400;
    for (i, &word) in handler_code.iter().enumerate() {
        let addr = HANDLER_ADDR + (i as u32 * 2);
        write_word(&mut cpu, addr, word);
    }

    // Set exception vector for level 7 (autovector base 0x64 + (7-1)*4 = 0x7C)
    write_long(&mut cpu, 0x7C, HANDLER_ADDR);

    // Set interrupt mask to 0 (allows all interrupts)
    cpu.regs.sr.set_int_prio_mask(0);

    // Step 1: Execute NOP with level 7 interrupt pending (transition 0→7)
    // This should jump to the handler
    cpu.bus.irq_level = Some(7);
    cpu.step().unwrap();
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "First NMI should enter handler (0→7 transition)"
    );

    // Step 2: Execute NOP in handler with level 7 STILL held high
    // This should NOT re-trigger (no transition, still at 7)
    cpu.step().unwrap();
    assert_eq!(
        cpu.regs.pc,
        HANDLER_ADDR + 2,
        "Level 7 held continuously should NOT re-trigger (no transition)"
    );

    // Step 3: Execute RTE, returning to main program
    cpu.step().unwrap();
    assert_eq!(
        cpu.regs.pc,
        INITIAL_PC + 2,
        "RTE should return to main program"
    );
    assert_eq!(
        cpu.regs.isp, INITIAL_SSP,
        "Stack should be unwound after RTE"
    );

    // Step 4: Drop interrupt level to 0, then execute NOP
    cpu.bus.irq_level = None;
    cpu.step().unwrap();
    assert_eq!(
        cpu.regs.pc,
        INITIAL_PC + 4,
        "Should execute next instruction with no interrupt"
    );

    // Step 5: Raise level back to 7 (transition 0→7), execute NOP
    // This SHOULD trigger another interrupt (new rising edge)
    cpu.bus.irq_level = Some(7);
    cpu.step().unwrap();
    assert_eq!(
        cpu.regs.pc, HANDLER_ADDR,
        "Second NMI should fire on new 0→7 transition"
    );
}

#[test]
fn test_single_interrupts() {
    // Test all combinations of interrupt priority mask (0-7) vs interrupt pending level (0-7)
    for interrupt_priority_mask in 0..=7 {
        for interrupt_pending_level in 0..=7 {
            test_single_interrupt(interrupt_priority_mask, interrupt_pending_level);
        }
    }
}

#[test]
fn test_multiple_interrupts_all() {
    // Test multiple interrupts: a higher priority interrupt should interrupt a lower priority one
    for interrupt_priority_mask in 0..=7 {
        for interrupt_pending_level2 in 0..=7 {
            for interrupt_pending_level1 in 0..=7 {
                test_multiple_interrupts(
                    interrupt_priority_mask,
                    interrupt_pending_level1,
                    interrupt_pending_level2,
                );
            }
        }
    }
}
