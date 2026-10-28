//! 68030 bus-cycle fault frame (format $B) vectors, ported for Snow from the
//! m68k-rs test suite (tests/bus_fault_frame_030_tests.rs): a user-mode data
//! read through an invalid page descriptor vectors through vector 2 with a
//! resumable frame, and the RTE continuation protocol (DF set reruns the
//! instruction; DF clear completes the data cycle from the frame buffers).
//!
//! The DF-cleared vectors are `#[ignore]`d: Snow does not model the
//! fault-completion half of the protocol (data input/output buffer supply),
//! replaying the whole instruction instead, which is safe for RAM targets.
//!
//! Portions derived from m68k-rs (https://github.com/benletchford/m68k-rs),
//! Copyright (c) 2026 Ben Letchford, used under the MIT License:
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions:
//!
//! The above copyright notice and this permission notice shall be included
//! in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//! OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//! IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//! CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
//! TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
//! SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg};
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestBus = Testbus<Address, u8, Bus68020>;
type TestCpu = CpuM68030Fpu<TestBus>;

const SSP: Address = 0x1F00;
const USP: Address = 0x1800;
const CODE: Address = 0x0100;
const HANDLER: Address = 0x0300;
const ROOT_TABLE: Address = 0x8000;
const B_TABLE: Address = 0x8400;
/// Logical test page: with TIA=8/TIB=9 (32 KB pages) this walks root
/// index 0, B index 2, so its descriptor is the third B-table entry.
const FAULT_PAGE: Address = 0x0001_0000;
const FAULT_DESC: Address = B_TABLE + 8;

/// Frame field offsets from the post-exception supervisor SP (M68030UM
/// 8.2): format $B is 46 words with SSW at +$0A, the data-cycle fault
/// address at +$10, the data output buffer at +$18 and the data input
/// buffer at +$2C.
const FRAME_LEN: Address = 0x5C;
const F_PC: Address = 0x02;
const F_FMT: Address = 0x06;
const F_SSW: Address = 0x0A;
const F_FA: Address = 0x10;
const F_DOB: Address = 0x18;
const F_DIB: Address = 0x2C;

fn write_word(bus: &mut TestBus, addr: Address, val: Word) {
    bus.mem.insert(addr, (val >> 8) as u8);
    bus.mem.insert(addr + 1, val as u8);
}

fn write_long(bus: &mut TestBus, addr: Address, val: Long) {
    bus.mem.insert(addr, (val >> 24) as u8);
    bus.mem.insert(addr + 1, (val >> 16) as u8);
    bus.mem.insert(addr + 2, (val >> 8) as u8);
    bus.mem.insert(addr + 3, val as u8);
}

fn read_word(bus: &TestBus, addr: Address) -> Word {
    let hi = *bus.mem.get(&addr).unwrap_or(&0) as Word;
    let lo = *bus.mem.get(&(addr + 1)).unwrap_or(&0) as Word;
    (hi << 8) | lo
}

fn read_long(bus: &TestBus, addr: Address) -> Long {
    let hi = read_word(bus, addr) as Long;
    let lo = read_word(bus, addr + 2) as Long;
    (hi << 16) | lo
}

/// A 68030 in user mode with translation enabled through a two-level table
/// (TC: E=1, IS=0, TIA=8, TIB=9 -> 32 KB pages). The first 64 KB (code,
/// stack, vectors, the tables themselves) is identity-mapped through two
/// page descriptors; FAULT_PAGE's descriptor holds `fault_desc`. Vector 2
/// points at HANDLER, whose code the individual test writes.
fn user_mode_030_with_table(fault_desc: Long) -> TestCpu {
    let bus = Testbus::new(M68030_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);

    write_long(&mut cpu.bus, ROOT_TABLE, B_TABLE | 2); // root[0] -> B table (short)
    write_long(&mut cpu.bus, B_TABLE, 0x0000_0001); // B[0]: identity page at 0
    write_long(&mut cpu.bus, B_TABLE + 4, 0x0000_8001); // B[1]: identity page at 0x8000
    write_long(&mut cpu.bus, FAULT_DESC, fault_desc); // B[2]: page under test

    write_long(&mut cpu.bus, 8, HANDLER); // vector 2

    let mut crp = RootPointerReg(0);
    crp.set_table_addr(ROOT_TABLE >> 4);
    crp.set_dt(2); // short-format root descriptors
    crp.set_limit(0x7FFF);
    cpu.regs.pmmu.crp = crp;
    // E=1, PS=32 KB, IS=0, TIA=8, TIB=9.
    cpu.regs.pmmu.tc = TcReg(0x80F0_8900);
    cpu.pmmu_cache_ensure();

    cpu.regs.sr.set_supervisor(false);
    cpu.regs.isp = SSP; // supervisor stack for the fault frame
    cpu.regs.usp = USP;
    cpu.regs.a[0] = FAULT_PAGE;
    cpu
}

/// Points the CPU at CODE and refills the prefetch from the (just-written)
/// instruction stream. Call after writing the test's code words.
fn reload(cpu: &mut TestCpu) {
    cpu.set_pc(CODE).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");
}

fn frame_base(cpu: &TestCpu) -> Address {
    let sp = *cpu.regs.ssp();
    assert_eq!(sp, SSP - FRAME_LEN, "format $B frame is 46 words");
    sp
}

fn step_n(cpu: &mut TestCpu, n: usize) {
    for _ in 0..n {
        cpu.step().expect("step");
    }
}

/// A user-mode data read through an invalid page descriptor pushes a format
/// $B frame: vector 2, SSW = DF | RW | SIZ=long | FC=user data, the fault
/// address in the data-cycle fault address long, and the rolled-back
/// instruction's PC for the restart.
#[test]
fn translation_fault_pushes_resumable_format_b_frame() {
    let mut cpu = user_mode_030_with_table(0); // invalid descriptor

    write_word(&mut cpu.bus, CODE, 0x2010); // MOVE.L (A0),D0
    reload(&mut cpu);

    cpu.step().expect("fault step");
    assert!(cpu.regs.sr.supervisor(), "fault must enter supervisor mode");
    assert_eq!(cpu.regs.pc, HANDLER, "must vector through vector 2");

    let f = frame_base(&cpu);
    assert_eq!(
        read_word(&cpu.bus, f + F_FMT),
        0xB008,
        "format $B, vector 2"
    );
    assert_eq!(
        read_word(&cpu.bus, f + F_SSW),
        0x0141,
        "SSW = DF | RW=read | SIZ=long | FC=user data"
    );
    assert_eq!(
        read_long(&cpu.bus, f + F_FA),
        FAULT_PAGE,
        "data fault address"
    );
    assert_eq!(read_long(&cpu.bus, f + F_PC), CODE, "restart PC");
}

/// The fix-and-rerun path: the vector-2 handler materializes the page
/// descriptor and RTEs with DF still set, and the restarted instruction
/// completes against the new mapping.
#[test]
fn rte_with_df_set_restarts_the_faulted_access() {
    let mut cpu = user_mode_030_with_table(0);

    write_long(&mut cpu.bus, 0x9000, 0xCAFE_F00D); // data in the to-be-mapped page
    write_word(&mut cpu.bus, CODE, 0x2010); // MOVE.L (A0),D0
    write_word(&mut cpu.bus, CODE + 2, 0x4E71); // NOP

    // Handler materializes the page: FAULT_PAGE -> physical 0, so the
    // restarted read of logical 0x10000 resolves to physical 0.
    // MOVE.L #$00000001,(FAULT_DESC).L ; descriptor: page at 0, DT=1
    write_word(&mut cpu.bus, HANDLER, 0x23FC);
    write_long(&mut cpu.bus, HANDLER + 2, 0x0000_0001);
    write_long(&mut cpu.bus, HANDLER + 6, FAULT_DESC);
    write_word(&mut cpu.bus, HANDLER + 10, 0x4E73); // RTE
    reload(&mut cpu);

    // Fault + handler (2 instructions) + rerun + NOP.
    step_n(&mut cpu, 5);
    assert!(!cpu.regs.sr.supervisor(), "back in user mode after RTE");
    assert_eq!(cpu.regs.pc, CODE + 4, "past the NOP");
    // Logical 0x10000 now maps to physical 0: D0 = longword at 0 (only
    // vector 2 was ever written there, so physical 0 reads 0).
    assert_eq!(
        cpu.regs.d[0],
        read_long(&cpu.bus, 0),
        "read completed via new mapping"
    );
}

/// The DF-cleared read path (mmu.library's lazily zero-filled pages): the
/// handler supplies the faulted read's result in the data input buffer,
/// clears DF, and RTEs; the instruction completes with that value while the
/// page stays invalid.
///
/// Ignored: Snow does not model fault completion through the frame buffers.
#[test]
#[ignore = "Snow replays the whole instruction on a format-$B RTE; the DF-cleared data-input-buffer completion path is not modeled"]
fn rte_with_df_cleared_supplies_the_data_input_buffer() {
    let mut cpu = user_mode_030_with_table(0);

    write_word(&mut cpu.bus, CODE, 0x2010); // MOVE.L (A0),D0
    write_word(&mut cpu.bus, CODE + 2, 0x4E71); // NOP

    // Handler: BCLR #0,($A,A7); MOVE.L #$CAFEBABE,($2C,A7); RTE
    write_word(&mut cpu.bus, HANDLER, 0x08AF);
    write_word(&mut cpu.bus, HANDLER + 2, 0x0000);
    write_word(&mut cpu.bus, HANDLER + 4, F_SSW as u16);
    write_word(&mut cpu.bus, HANDLER + 6, 0x2F7C);
    write_long(&mut cpu.bus, HANDLER + 8, 0xCAFE_BABE);
    write_word(&mut cpu.bus, HANDLER + 12, F_DIB as u16);
    write_word(&mut cpu.bus, HANDLER + 14, 0x4E73); // RTE
    reload(&mut cpu);

    // Fault + handler (3 instructions) + completed re-execution + NOP.
    step_n(&mut cpu, 6);
    assert!(!cpu.regs.sr.supervisor());
    assert_eq!(cpu.regs.d[0], 0xCAFE_BABE, "read completed from the DIB");
    assert_eq!(
        read_long(&cpu.bus, FAULT_DESC),
        0,
        "page descriptor stays invalid"
    );
}

/// The DF-cleared write path: a faulted write stacks its value in the data
/// output buffer; a handler that clears DF absorbs the write, and the
/// re-executed instruction does not redo (or re-fault) it.
///
/// Ignored: Snow does not model write absorption through the DOB.
#[test]
#[ignore = "Snow replays the whole instruction on a format-$B RTE; the DF-cleared write-absorption path (data output buffer) is not modeled"]
fn rte_with_df_cleared_absorbs_the_faulted_write() {
    let mut cpu = user_mode_030_with_table(0);

    cpu.regs.d[0] = 0x1234_5678;
    write_word(&mut cpu.bus, CODE, 0x2080); // MOVE.L D0,(A0)
    write_word(&mut cpu.bus, CODE + 2, 0x4E71); // NOP

    // Handler: MOVE.L ($18,A7),D7; BCLR #0,($A,A7); RTE
    write_word(&mut cpu.bus, HANDLER, 0x2E2F);
    write_word(&mut cpu.bus, HANDLER + 2, F_DOB as u16);
    write_word(&mut cpu.bus, HANDLER + 4, 0x08AF);
    write_word(&mut cpu.bus, HANDLER + 6, 0x0000);
    write_word(&mut cpu.bus, HANDLER + 8, F_SSW as u16);
    write_word(&mut cpu.bus, HANDLER + 10, 0x4E73); // RTE
    reload(&mut cpu);

    // Check the frame SSW marks a write before running the handler.
    cpu.step().expect("fault");
    let f = frame_base(&cpu);
    assert_eq!(
        read_word(&cpu.bus, f + F_SSW),
        0x0101,
        "SSW = DF | RW=write | SIZ=long | FC=user data"
    );
    assert_eq!(
        read_long(&cpu.bus, f + F_DOB),
        0x1234_5678,
        "data output buffer holds the faulted write's value"
    );

    step_n(&mut cpu, 5); // handler (3) + suppressed rerun + NOP
    assert!(!cpu.regs.sr.supervisor());
    assert_eq!(cpu.regs.d[7], 0x1234_5678, "handler saw the write data");
    assert_eq!(cpu.regs.pc, CODE + 4, "no re-fault loop");
}

/// PTEST reports accumulated write-protect on the walked path in MMUSR W,
/// for read tests too (M68030UM Table 9-3: W is set when any descriptor
/// encountered during the search has WP set).
#[test]
fn ptest_reports_write_protect_on_read_walks() {
    let mut cpu = user_mode_030_with_table(0x0000_0005); // DT=1 | WP

    cpu.regs.sr.set_supervisor(true);
    cpu.regs.isp = SSP;
    cpu.regs.a[0] = FAULT_PAGE;
    cpu.regs.d[0] = 1;
    write_word(&mut cpu.bus, CODE, 0x4E7B); // MOVEC D0,SFC
    write_word(&mut cpu.bus, CODE + 2, 0x0000);
    write_word(&mut cpu.bus, CODE + 4, 0xF010); // PTESTR SFC,(A0),#7
    write_word(&mut cpu.bus, CODE + 6, 0x9E00);
    cpu.set_pc(CODE).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");

    step_n(&mut cpu, 2);
    assert_eq!(
        cpu.regs.pmmu.psr.0, 0x0802,
        "MMUSR = W | N=2 for a resident write-protected page"
    );
}

/// PTEST completes a search that finds a supervisor-only long-format page
/// descriptor and reports S in MMUSR (M68030UM Table 9-3) instead of
/// aborting; a real user access to the same page still faults.
#[test]
fn ptest_reports_supervisor_only_on_user_probe() {
    let bus = Testbus::new(M68030_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);

    const ROOT: Address = 0x8000;
    const B: Address = 0x8800;
    const PAGE: Address = 0x0001_0000; // root idx 0, B idx 2
    // Long-format root descriptor (DT=3, LU=1: no lower-limit violation)
    // -> long-format B table.
    write_long(&mut cpu.bus, ROOT, 0x8000_0003);
    write_long(&mut cpu.bus, ROOT + 4, B);
    // B[0]: identity page 0. B[1]: identity page 0x8000.
    write_long(&mut cpu.bus, B, 0x0000_0001);
    write_long(&mut cpu.bus, B + 4, 0x0000_0000);
    write_long(&mut cpu.bus, B + 8, 0x0000_0001);
    write_long(&mut cpu.bus, B + 12, 0x0000_8000);
    // B[2]: long page descriptor, DT=1, S set, page 0.
    write_long(&mut cpu.bus, B + 16, 0x0000_0101);
    write_long(&mut cpu.bus, B + 20, 0x0000_0000);

    let mut crp = RootPointerReg(0);
    crp.set_table_addr(ROOT >> 4);
    crp.set_dt(3); // long-format root descriptors
    crp.set_limit(0x7FFF);
    cpu.regs.pmmu.crp = crp;
    // E=1, IS=0, TIA=8, TIB=9, PS=32 KB.
    cpu.regs.pmmu.tc = TcReg(0x80F0_8900);
    cpu.pmmu_cache_ensure();

    cpu.regs.sr.set_supervisor(true);
    cpu.regs.isp = SSP;
    cpu.regs.a[0] = PAGE;
    cpu.regs.d[0] = 1; // SFC = 1: user data space
    write_word(&mut cpu.bus, CODE, 0x4E7B); // MOVEC D0,SFC
    write_word(&mut cpu.bus, CODE + 2, 0x0000);
    write_word(&mut cpu.bus, CODE + 4, 0xF010); // PTESTR SFC,(A0),#7
    write_word(&mut cpu.bus, CODE + 6, 0x9E00);
    cpu.set_pc(CODE).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");

    cpu.step().expect("MOVEC");
    cpu.step().expect("PTESTR");
    assert_eq!(
        cpu.regs.pmmu.psr.0, 0x2002,
        "MMUSR = S | N=2: supervisor-only page probed from user space"
    );
}

/// PTEST with the A bit loads An with the address of the last descriptor
/// fetched during the search, even when the search finds an invalid
/// descriptor (M68030UM 9.8).
#[test]
fn ptest_a_bit_returns_last_descriptor_on_failure() {
    let mut cpu = user_mode_030_with_table(0); // B[2] invalid

    cpu.regs.sr.set_supervisor(true);
    cpu.regs.isp = SSP;
    cpu.regs.a[0] = FAULT_PAGE;
    cpu.regs.d[0] = 1; // SFC = 1
    write_word(&mut cpu.bus, CODE, 0x4E7B); // MOVEC D0,SFC
    write_word(&mut cpu.bus, CODE + 2, 0x0000);
    write_word(&mut cpu.bus, CODE + 4, 0xF010); // PTESTR SFC,(A0),#2,A1
    write_word(&mut cpu.bus, CODE + 6, 0x8B20);
    reload(&mut cpu);

    step_n(&mut cpu, 2);
    assert_eq!(
        cpu.regs.a[1], FAULT_DESC,
        "A1 holds the invalid descriptor's address"
    );
    assert_eq!(
        cpu.regs.pmmu.psr.0 & 0x0400,
        0x0400,
        "MMUSR I set for the invalid translation"
    );
}

/// PTEST with a level operand ends the search at that level (M68000PRM
/// 6-62): after one level on a two-level tree the search stops on the valid
/// root descriptor and reports N=1 without I, and the A bit returns the last
/// successfully fetched descriptor.
#[test]
fn ptest_level_limit_stops_the_search() {
    let mut cpu = user_mode_030_with_table(0);

    cpu.regs.sr.set_supervisor(true);
    cpu.regs.isp = SSP;
    cpu.regs.a[0] = FAULT_PAGE;
    cpu.regs.d[0] = 1; // SFC = 1
    write_word(&mut cpu.bus, CODE, 0x4E7B); // MOVEC D0,SFC
    write_word(&mut cpu.bus, CODE + 2, 0x0000);
    write_word(&mut cpu.bus, CODE + 4, 0xF010); // PTESTR SFC,(A0),#1,A1
    write_word(&mut cpu.bus, CODE + 6, 0x8720);
    reload(&mut cpu);

    step_n(&mut cpu, 2);
    assert_eq!(
        cpu.regs.pmmu.psr.0, 0x0001,
        "MMUSR = N=1, no I: the search stopped on the root descriptor"
    );
    assert_eq!(
        cpu.regs.a[1], ROOT_TABLE,
        "A bit returns the last successfully fetched descriptor"
    );
}

/// MOVES in user mode is a privilege violation regardless of direction.
#[test]
fn moves_is_privileged() {
    let bus = Testbus::new(M68030_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);
    write_long(&mut cpu.bus, 8 * 4, HANDLER); // privilege violation vector

    cpu.regs.sr.set_supervisor(false);
    cpu.regs.isp = SSP; // supervisor stack for the exception frame
    cpu.regs.usp = USP;
    cpu.regs.a[0] = 0x2000;
    write_word(&mut cpu.bus, CODE, 0x0E10); // MOVES.B (A0),D1
    write_word(&mut cpu.bus, CODE + 2, 0x1000);
    cpu.set_pc(CODE).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");

    cpu.step().expect("step");
    assert!(cpu.regs.sr.supervisor(), "privilege violation taken");
    assert_eq!(cpu.regs.pc, HANDLER);
    assert_eq!(
        read_long(&cpu.bus, *cpu.regs.ssp() + 2),
        CODE,
        "faulting instruction's PC stacked"
    );
}
