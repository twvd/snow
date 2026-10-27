//! 68030 PMMU translation vectors, ported for Snow from the m68k-rs test
//! suite (tests/mmu_translate_tests.rs, tests/mmu_page_crossing_tests.rs):
//! identity walks, remapping, invalid-descriptor faults, and misaligned
//! accesses that straddle a page boundary where the two virtual pages map
//! to non-adjacent physical pages.
//!
//! The 68040-only vectors upstream were dropped (Snow is a 68030); the
//! page-crossing scenarios were re-expressed on a 68030 short-format table.
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
use crate::bus::testbus::{Access, Testbus};
use crate::cpu_m68k::bus::{FC_SUPERVISOR_DATA, FC_SUPERVISOR_PROGRAM};
use crate::cpu_m68k::cpu::CpuError;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg};
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestBus = Testbus<Address, u8, Bus68020>;
type TestCpu = CpuM68030Fpu<TestBus>;

/// E=1, PS=4 KB, IS=8, TIA=12: one level of 4096 short descriptors indexed
/// by logical bits 23:12. IS+TIA+PS = 8+12+12 = 32.
const TC: u32 = 0x80C8_C000;
/// Physical address of the short-format root table.
const TABLE: Address = 0x1_0000;
/// Code placed here for instruction-based tests (identity-mapped page 0).
const CODE: Address = 0x100;

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

/// Supervisor-mode 68030 with `pages` 4 KB pages identity-mapped through a
/// short-format root table at `table`, and the PMMU enabled with `tc`.
///
/// The root pointer's upper limit is set to 0x7FFF so the limit field never
/// rejects an index (m68k-rs does not model root-pointer limits at all).
fn mmu_on(cpu: &mut TestCpu, tc: u32, table: Address, pages: u32) {
    for page in 0..pages {
        write_long(&mut cpu.bus, table + page * 4, (page << 12) | 1);
    }
    let mut crp = RootPointerReg(0);
    crp.set_table_addr(table >> 4);
    crp.set_dt(2); // short-format (4-byte) descriptors
    crp.set_limit(0x7FFF); // upper limit, never violated below
    cpu.regs.pmmu.crp = crp;
    cpu.regs.pmmu.tc = TcReg(tc);
    cpu.pmmu_cache_ensure();
}

fn testcpu() -> TestCpu {
    let bus = Testbus::new(M68030_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    cpu
}

/// Counts bus reads that landed inside `start..end` (the table region).
/// Each 32-bit descriptor read appears as four byte-level trace entries on
/// the 32-bit test bus.
fn table_reads(cpu: &TestCpu, start: Address, end: Address) -> usize {
    cpu.bus
        .get_trace()
        .iter()
        .filter(|t| t.access == Access::Read && (start..end).contains(&t.addr))
        .count()
        / 4
}

/// One translated supervisor-data access.
fn data(cpu: &mut TestCpu, logical: Address) -> Address {
    cpu.pmmu_translate(FC_SUPERVISOR_DATA, logical, false)
        .expect("translation must succeed")
}

/// Runs the one instruction encoded by `words` at CODE.
fn run(cpu: &mut TestCpu, words: &[Word]) {
    for (i, &w) in words.iter().enumerate() {
        write_word(&mut cpu.bus, CODE + i as Address * 2, w);
    }
    cpu.set_pc(CODE).expect("set_pc");
    cpu.prefetch_refill().expect("prefetch");
    cpu.step().expect("step");
}

#[test]
fn identity_walk_translates_and_caches_per_page() {
    let mut cpu = testcpu();
    mmu_on(&mut cpu, TC, TABLE, 32);

    cpu.bus.reset_trace();
    assert_eq!(data(&mut cpu, 0x3010), 0x3010);
    assert_eq!(table_reads(&cpu, TABLE, TABLE + 0x1000), 1, "first walk");

    assert_eq!(data(&mut cpu, 0x3FFC), 0x3FFC);
    assert_eq!(table_reads(&cpu, TABLE, TABLE + 0x1000), 1, "ATC hit");

    assert_eq!(data(&mut cpu, 0x4000), 0x4000);
    assert_eq!(table_reads(&cpu, TABLE, TABLE + 0x1000), 2, "next page");
}

#[test]
fn remap_redirects_to_physical_page() {
    let mut cpu = testcpu();
    mmu_on(&mut cpu, TC, TABLE, 32);
    // Page 1 (logical 0x1000) -> physical 0x8000.
    write_long(&mut cpu.bus, TABLE + 4, 0x8000 | 1);

    assert_eq!(data(&mut cpu, 0x1000), 0x8000);
}

#[test]
fn invalid_descriptor_faults_data_and_program_fc_alike() {
    // No valid descriptors: any access through the table faults, whether it
    // is a data access (FC 5) or an instruction fetch (FC 6).
    let mut cpu = testcpu();
    mmu_on(&mut cpu, TC, TABLE, 0);

    let err = cpu.pmmu_translate(FC_SUPERVISOR_DATA, 0x1000, false);
    let data_err = err.unwrap_err().downcast::<CpuError>().expect("CpuError");
    assert!(matches!(data_err, CpuError::BusError(_)), "{data_err:?}");

    let err = cpu.pmmu_translate(FC_SUPERVISOR_PROGRAM, 0x1000, false);
    let fetch_err = err.unwrap_err().downcast::<CpuError>().expect("CpuError");
    assert!(matches!(fetch_err, CpuError::BusError(_)), "{fetch_err:?}");
}

/// The m68k-rs page-crossing scenario on a 68030: logical pages 5 and 6 are
/// adjacent but map to non-adjacent physical pages (0x6000 and 0xA000). A
/// long read straddling the 0x6000 logical boundary must fetch each half
/// through its own translation; a flat base-translated access would read
/// the physically adjacent page (0x7000) instead.
#[test]
fn page_crossing_long_read_translates_each_half() {
    let mut cpu = testcpu();
    mmu_on(&mut cpu, TC, TABLE, 32);
    write_long(&mut cpu.bus, TABLE + 5 * 4, 0x6000 | 1);
    write_long(&mut cpu.bus, TABLE + 6 * 4, 0xA000 | 1);

    write_word(&mut cpu.bus, 0x6FFE, 0xDEAD);
    write_word(&mut cpu.bus, 0xA000, 0xBEEF);
    write_word(&mut cpu.bus, 0x7000, 0x7777); // physically adjacent decoy

    cpu.regs.a[0] = 0x5FFE;
    run(&mut cpu, &[0x2010]); // MOVE.L (A0),D0

    assert_eq!(
        cpu.regs.d[0], 0xDEAD_BEEF,
        "low half must come from the mapped page, not the physically adjacent one"
    );
}

#[test]
fn page_crossing_long_write_translates_each_half() {
    let mut cpu = testcpu();
    mmu_on(&mut cpu, TC, TABLE, 32);
    write_long(&mut cpu.bus, TABLE + 5 * 4, 0x6000 | 1);
    write_long(&mut cpu.bus, TABLE + 6 * 4, 0xA000 | 1);

    cpu.regs.a[0] = 0x5FFE;
    cpu.regs.d[0] = 0xCAFE_F00D;
    run(&mut cpu, &[0x2080]); // MOVE.L D0,(A0)

    assert_eq!(read_word(&cpu.bus, 0x6FFE), 0xCAFE, "high half");
    assert_eq!(read_word(&cpu.bus, 0xA000), 0xF00D, "low half");
    assert_eq!(
        read_word(&cpu.bus, 0x7000),
        0,
        "nothing may spill onto the physically adjacent page"
    );
}

#[test]
fn write_protected_page_reads_ok_and_writes_fault() {
    let mut cpu = testcpu();
    mmu_on(&mut cpu, TC, TABLE, 32);
    // Page 3 with WP set (bit 2 of the descriptor).
    write_long(&mut cpu.bus, TABLE + 3 * 4, (3 << 12) | 1 | 4);

    assert_eq!(data(&mut cpu, 0x3000), 0x3000, "read translates");

    let err = cpu
        .pmmu_translate(FC_SUPERVISOR_DATA, 0x3000, true)
        .unwrap_err()
        .downcast::<CpuError>()
        .expect("CpuError");
    match err {
        CpuError::BusError(details) => {
            assert!(!details.read, "write fault must be marked as a write");
            assert_eq!(details.address, 0x3000);
        }
        other => panic!("expected bus error, got {other:?}"),
    }
    assert!(
        cpu.regs.pmmu.psr.write_protected(),
        "PSR must report the write-protect cause"
    );
}
