//! PMOVE TTx register vectors, ported for Snow from the m68k-rs test suite
//! (tests/test_pmove_tt_registers.rs): the 68030 PMOVE format-2 encoding
//! must treat TT0/TT1 as 32-bit registers -- a 3-bit preg decode would
//! misread them as the 64-bit SRP/CRP and consume 8 bytes.
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
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestBus = Testbus<Address, u8, Bus68020>;
type TestCpu = CpuM68030Fpu<TestBus>;

const CODE: Address = 0x0200;
const DATA: Address = 0x2000;

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

fn read_long(bus: &TestBus, addr: Address) -> Long {
    let b0 = *bus.mem.get(&addr).unwrap_or(&0) as Long;
    let b1 = *bus.mem.get(&(addr + 1)).unwrap_or(&0) as Long;
    let b2 = *bus.mem.get(&(addr + 2)).unwrap_or(&0) as Long;
    let b3 = *bus.mem.get(&(addr + 3)).unwrap_or(&0) as Long;
    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
}

/// A supervisor-mode 68030 with code at CODE (the MMU stays disabled: these
/// vectors test PMOVE decode, not translation).
fn cpu_030() -> TestCpu {
    let bus = Testbus::new(M68030_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu.regs.isp = 0x1000;
    cpu
}

/// Runs the one instruction at CODE.
fn run(cpu: &mut TestCpu, words: &[Word]) {
    for (i, &w) in words.iter().enumerate() {
        write_word(&mut cpu.bus, CODE + i as Address * 2, w);
    }
    cpu.set_pc(CODE).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");
    cpu.step().expect("step");
}

#[test]
fn pmove_tt0_load_reads_32_bits_not_64() {
    let mut cpu = cpu_030();

    write_long(&mut cpu.bus, DATA, 0x0000_8021); // TT0 value
    write_long(&mut cpu.bus, DATA + 4, 0xDEAD_BEEF); // sentinel, must not be read

    cpu.regs.a[0] = DATA;
    run(&mut cpu, &[0xF010, 0x0800]); // PMOVE (A0),TT0

    assert_eq!(
        cpu.regs.pmmu.tt[0].0, 0x0000_8021,
        "PMOVE (A0),TT0 loads a 32-bit TT0"
    );
    // SRP must not have been touched (it would be if preg 2 were decoded as
    // the 64-bit SRP).
    assert_eq!(cpu.regs.pmmu.srp.0, 0, "SRP untouched by PMOVE TT0");
}

#[test]
fn pmove_tt1_load_reads_32_bits_not_64() {
    let mut cpu = cpu_030();

    write_long(&mut cpu.bus, DATA, 0x0000_8043); // TT1 value
    write_long(&mut cpu.bus, DATA + 4, 0xCAFE_BABE); // sentinel

    cpu.regs.a[0] = DATA;
    run(&mut cpu, &[0xF010, 0x0C00]); // PMOVE (A0),TT1

    assert_eq!(
        cpu.regs.pmmu.tt[1].0, 0x0000_8043,
        "PMOVE (A0),TT1 loads a 32-bit TT1"
    );
    // CRP must not have been touched (preg 3 decoded as the 64-bit CRP).
    assert_eq!(cpu.regs.pmmu.crp.0, 0, "CRP untouched by PMOVE TT1");
}

#[test]
fn pmove_tt0_store_writes_32_bits() {
    let mut cpu = cpu_030();

    cpu.regs.pmmu.tt[0].0 = 0x1234_5678;
    cpu.regs.a[0] = DATA;
    run(&mut cpu, &[0xF010, 0x0A00]); // PMOVE TT0,(A0)

    assert_eq!(read_long(&cpu.bus, DATA), 0x1234_5678);
    assert_eq!(
        read_long(&cpu.bus, DATA + 4),
        0,
        "only 4 bytes are written for TT0, not 8"
    );
}

#[test]
fn pmove_tt1_store_writes_32_bits() {
    let mut cpu = cpu_030();

    cpu.regs.pmmu.tt[1].0 = 0xABCD_EF01;
    cpu.regs.a[0] = DATA;
    run(&mut cpu, &[0xF010, 0x0E00]); // PMOVE TT1,(A0)

    assert_eq!(read_long(&cpu.bus, DATA), 0xABCD_EF01);
    assert_eq!(read_long(&cpu.bus, DATA + 4), 0, "only 4 bytes, not 8");
}

/// The full EmuTOS-style MMU setup sequence: CRP (64-bit), TC (32-bit),
/// TT0 (32-bit), TT1 (32-bit). With a 3-bit preg decode, TT0 would be
/// misidentified as SRP and TT1 as CRP, corrupting CRP.
#[test]
fn pmove_full_mmu_setup_sequence() {
    let mut cpu = cpu_030();

    // Data area: CRP limit, CRP aptr, TC (bit 31 clear: MMU stays off),
    // TT0, TT1.
    let crp_limit: Long = 0x7FFF_0002;
    let crp_aptr: Long = 0x0070_0000;
    let tc_value: Long = 0x0071_0507;
    let tt0_value: Long = 0x0000_8021;
    let tt1_value: Long = 0x0000_8043;

    write_long(&mut cpu.bus, DATA, crp_limit);
    write_long(&mut cpu.bus, DATA + 4, crp_aptr);
    write_long(&mut cpu.bus, DATA + 8, tc_value);
    write_long(&mut cpu.bus, DATA + 12, tt0_value);
    write_long(&mut cpu.bus, DATA + 16, tt1_value);

    let pc = CODE;
    // PMOVE (DATA).L,CRP
    write_word(&mut cpu.bus, pc, 0xF039);
    write_word(&mut cpu.bus, pc + 2, 0x4C00);
    write_long(&mut cpu.bus, pc + 4, DATA);
    // PMOVE (DATA+8).L,TC
    write_word(&mut cpu.bus, pc + 8, 0xF039);
    write_word(&mut cpu.bus, pc + 10, 0x4000);
    write_long(&mut cpu.bus, pc + 12, DATA + 8);
    // PMOVE (DATA+12).L,TT0
    write_word(&mut cpu.bus, pc + 16, 0xF039);
    write_word(&mut cpu.bus, pc + 18, 0x0800);
    write_long(&mut cpu.bus, pc + 20, DATA + 12);
    // PMOVE (DATA+16).L,TT1
    write_word(&mut cpu.bus, pc + 24, 0xF039);
    write_word(&mut cpu.bus, pc + 26, 0x0C00);
    write_long(&mut cpu.bus, pc + 28, DATA + 16);

    cpu.set_pc(pc).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");

    cpu.step().expect("CRP PMOVE");
    assert_eq!(cpu.regs.pmmu.crp.limit(), 0x7FFF, "CRP limit");
    assert_eq!(cpu.regs.pmmu.crp.table_addr() << 4, crp_aptr, "CRP aptr");

    cpu.step().expect("TC PMOVE");
    assert_eq!(cpu.regs.pmmu.tc.0, tc_value, "TC");

    cpu.step().expect("TT0 PMOVE");
    assert_eq!(cpu.regs.pmmu.tt[0].0, tt0_value, "TT0");

    cpu.step().expect("TT1 PMOVE");
    assert_eq!(cpu.regs.pmmu.tt[1].0, tt1_value, "TT1");

    // CRP must not have been corrupted by the TT1 instruction.
    assert_eq!(cpu.regs.pmmu.crp.limit(), 0x7FFF, "CRP limit intact");
    assert_eq!(
        cpu.regs.pmmu.crp.table_addr() << 4,
        crp_aptr,
        "CRP aptr intact"
    );
    // SRP untouched (TT0 misdecoded as SRP would have changed it).
    assert_eq!(cpu.regs.pmmu.srp.0, 0, "SRP untouched");
}
