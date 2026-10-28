//! 68030 ATC and PMMU-instruction vectors, ported for Snow from the m68k-rs
//! test suite (tests/mmu_030_atc_tests.rs): per-page ATC caching, early
//! termination, PFLUSH/PLOAD/PMOVE flush semantics, and write-protect
//! enforcement on ATC hits. Encodings cross-checked against the M68000
//! Family Programmer's Reference Manual (6-32..6-34) and the M68030 UM.
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
use crate::cpu_m68k::bus::{FC_SUPERVISOR_DATA, FC_SUPERVISOR_PROGRAM, FC_USER_DATA};
use crate::cpu_m68k::cpu::CpuError;
use crate::cpu_m68k::pmmu::regs::{RootPointerReg, TcReg, TrTranslationReg};
use crate::cpu_m68k::pmmu::translate::PMMU_ATC_URP;
use crate::cpu_m68k::{CpuM68030Fpu, M68030_ADDRESS_MASK};
use crate::types::{Long, Word};

type TestBus = Testbus<Address, u8, Bus68020>;
type TestCpu = CpuM68030Fpu<TestBus>;

/// E=1, PS=4 KB, IS=8, TIA=12: one level of 4096 short descriptors.
const TC: u32 = 0x80C8_C000;
const TABLE: Address = 0x1_0000;
/// Reads from here up to TABLE_END count as table reads.
const TABLE_END: Address = 0x2_0000;
const CODE: Address = 0x1000;

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

/// Map `page` to `phys_page` (DT=1 page descriptor) in the root table.
fn map(bus: &mut TestBus, page: u32, phys_page: u32) {
    write_long(bus, TABLE + page * 4, (phys_page << 12) | 1);
}

fn mmu_on(cpu: &mut TestCpu, tc: u32, table: Address, pages: u32) {
    for page in 0..pages {
        map(&mut cpu.bus, page, page);
    }
    let mut crp = RootPointerReg(0);
    crp.set_table_addr(table >> 4);
    crp.set_dt(2); // short-format descriptors
    crp.set_limit(0x7FFF); // upper limit, never violated below
    cpu.regs.pmmu.crp = crp;
    cpu.regs.pmmu.tc = TcReg(tc);
    cpu.pmmu_cache_ensure();
    cpu.bus.reset_trace();
}

/// Supervisor-mode 68030 with the first 32 4 KB pages identity-mapped.
fn cpu_mmu_030() -> TestCpu {
    let bus = Testbus::new(M68030_ADDRESS_MASK);
    let mut cpu = TestCpu::new(bus);
    cpu.regs.sr.set_supervisor(true);
    cpu.regs.sr.set_int_prio_mask(7);
    mmu_on(&mut cpu, TC, TABLE, 32);
    cpu
}

fn table_reads(cpu: &TestCpu) -> usize {
    cpu.bus
        .get_trace()
        .iter()
        .filter(|t| t.access == Access::Read && (TABLE..TABLE_END).contains(&t.addr))
        .count()
        / 4
}

fn data(cpu: &mut TestCpu, logical: Address) -> Address {
    cpu.pmmu_translate(FC_SUPERVISOR_DATA, logical, false)
        .expect("translation must succeed")
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

/// A repeat access to a page is answered without a table search. Snow's
/// ATC is direct-mapped and its slot for a page is shared across function
/// codes: translation does not depend on FC unless SRE or FCL are set, and
/// the hardware may drop ATC entries at any time, so serving FC 5, 6 and 1
/// from the same entry is legal (the hardware's fully associative ATC would
/// hold separate entries; Snow only differs in table-search counts).
#[test]
fn repeat_access_to_a_page_needs_no_table_search() {
    let mut cpu = cpu_mmu_030();

    assert_eq!(data(&mut cpu, 0x3010), 0x3010);
    assert_eq!(table_reads(&cpu), 1, "first access searches the table");

    assert_eq!(data(&mut cpu, 0x3FFC), 0x3FFC);
    assert_eq!(table_reads(&cpu), 1, "same page: ATC hit");

    assert_eq!(data(&mut cpu, 0x4000), 0x4000);
    assert_eq!(table_reads(&cpu), 2, "next page: another search");

    // Supervisor program space reuses the shared slot.
    cpu.pmmu_translate(FC_SUPERVISOR_PROGRAM, 0x3010, false)
        .expect("translate");
    assert_eq!(table_reads(&cpu), 2, "program space: shared-slot hit");

    // And so does the user data space.
    cpu.pmmu_translate(FC_USER_DATA, 0x3010, false)
        .expect("translate");
    assert_eq!(table_reads(&cpu), 2, "user data space: shared-slot hit");
}

/// A two-level tree whose level-A descriptor terminates early maps a whole
/// 1 MB region; each 4 KB page is still cached as its own entry. Per
/// M68030UM 9.5.3.1 the translated address is the sum of the descriptor's
/// page field and the offset, so the base need not be page-aligned.
#[test]
fn early_termination_descriptor_is_cached_one_page_at_a_time() {
    let mut cpu = cpu_mmu_030();
    let root: Address = 0x1_8000;
    // TC: E, PS=4 KB, IS=8, TIA=4, TIB=8 (IS+TIA+TIB+PS = 32).
    cpu.regs.pmmu.tc = TcReg(0x80C8_4800);
    cpu.pmmu_cache_ensure();
    let mut crp = RootPointerReg(0);
    crp.set_table_addr(root >> 4);
    crp.set_dt(2);
    crp.set_limit(0x7FFF);
    cpu.regs.pmmu.crp = crp;
    // Early-termination descriptor at each TIA index the test uses (the
    // upstream vector only writes index 0; the descriptors are spelled out
    // here).
    for logical in [0x0000_1234u32, 0x0000_1FFF, 0x000F_F000] {
        let idx = (logical >> 20) & 0xF;
        write_long(&mut cpu.bus, root + idx * 4, 0x0005_0001);
    }

    for logical in [0x0000_1234u32, 0x0000_1FFF, 0x000F_F000] {
        let walked = 0x0005_0000 + (logical & 0x000F_FFFF);
        assert_eq!(data(&mut cpu, logical), walked);
        cpu.bus.reset_trace();
        assert_eq!(data(&mut cpu, logical ^ 0x10), walked ^ 0x10);
        assert_eq!(table_reads(&cpu), 0, "{logical:#x} hits");
    }
}

/// PFLUSH #5,#7,(A0) flushes only page 5's supervisor-data entry; page 6's
/// entry survives until PFLUSH #4,#4 flushes every supervisor entry.
#[test]
fn stale_translation_lasts_until_pflush() {
    let mut cpu = cpu_mmu_030();
    assert_eq!(data(&mut cpu, 0x5004), 0x5004);
    assert_eq!(data(&mut cpu, 0x6004), 0x6004);

    // Remap both pages without a flush: the ATC keeps the old answers.
    map(&mut cpu.bus, 5, 0x9);
    map(&mut cpu.bus, 6, 0xA);
    assert_eq!(data(&mut cpu, 0x5004), 0x5004, "stale entry held");

    // PFLUSH #5,#7,(A0) with A0 in page 5: only that page's supervisor
    // data entry goes.
    cpu.regs.a[0] = 0x5000;
    run(&mut cpu, &[0xF010, 0x38F5]);
    assert_eq!(data(&mut cpu, 0x5004), 0x9004);
    assert_eq!(data(&mut cpu, 0x6004), 0x6004, "page 6 kept");

    // PFLUSH #4,#4: every supervisor entry.
    run(&mut cpu, &[0xF000, 0x3094]);
    assert_eq!(data(&mut cpu, 0x6004), 0xA004);
}

#[test]
fn pflusha_and_pmove_flush_but_pmovefd_does_not() {
    let mut cpu = cpu_mmu_030();
    assert_eq!(data(&mut cpu, 0x5004), 0x5004);
    map(&mut cpu.bus, 5, 0x9);

    // PFLUSHA
    run(&mut cpu, &[0xF000, 0x2400]);
    assert_eq!(data(&mut cpu, 0x5004), 0x9004);

    // PMOVEFD (A0),TC rewrites TC with the same value and keeps the ATC.
    write_long(&mut cpu.bus, 0x7000, TC);
    cpu.regs.a[0] = 0x7000;
    map(&mut cpu.bus, 5, 0xB);
    run(&mut cpu, &[0xF010, 0x4100]);
    assert_eq!(cpu.regs.pmmu.tc.0, TC);
    assert_eq!(data(&mut cpu, 0x5004), 0x9004, "FD kept the entry");

    // PMOVE (A0),TC flushes it.
    run(&mut cpu, &[0xF010, 0x4000]);
    assert_eq!(data(&mut cpu, 0x5004), 0xB004);

    // So does a PMOVE to CRP.
    write_long(&mut cpu.bus, 0x7008, 0x7FFF_0002);
    write_long(&mut cpu.bus, 0x700C, TABLE); // raw u64: LSL holds the full table address
    cpu.regs.a[0] = 0x7008;
    map(&mut cpu.bus, 5, 0xC);
    run(&mut cpu, &[0xF010, 0x4C00]);
    assert_eq!(data(&mut cpu, 0x5004), 0xC004);
}

#[test]
fn pmove_flush_follows_its_operand_fetch() {
    let mut cpu = cpu_mmu_030();
    // A second table maps page 7 somewhere else; the PMOVE (A0),CRP operand
    // lives there. Fetching the operand through the old root caches page
    // 7's old translation; the flush after the write must drop it.
    let new_root: Address = 0x1_4000;
    for page in 0..32u32 {
        let phys = if page == 7 { 0xD } else { page };
        write_long(&mut cpu.bus, new_root + page * 4, (phys << 12) | 1);
    }
    write_long(&mut cpu.bus, 0x7008, 0x7FFF_0002);
    write_long(&mut cpu.bus, 0x700C, new_root); // raw u64: LSL holds the full table address
    cpu.regs.a[0] = 0x7008;
    run(&mut cpu, &[0xF010, 0x4C00]); // PMOVE (A0),CRP
    assert_eq!(cpu.regs.pmmu.crp.table_addr(), new_root >> 4);
    assert_eq!(data(&mut cpu, 0x7004), 0xD004);
}

#[test]
fn pmove_operand_fetch_uses_the_cached_translation() {
    let mut cpu = cpu_mmu_030();
    assert_eq!(data(&mut cpu, 0x7004), 0x7004);
    // Edit page 7's descriptor without a flush: the operand fetch still goes
    // through the cached translation, as on the part, so PMOVE (A0),TC reads
    // the TC from the old page.
    map(&mut cpu.bus, 7, 0xE);
    write_long(&mut cpu.bus, 0x7000, TC);
    write_long(&mut cpu.bus, 0xE000, 0);
    cpu.regs.a[0] = 0x7000;
    run(&mut cpu, &[0xF010, 0x4000]);
    assert_eq!(cpu.regs.pmmu.tc.0, TC);
    assert!(cpu.regs.pmmu.tc.enable());
    // The write flushed the stale entry: the edit is now in force.
    assert_eq!(data(&mut cpu, 0x7004), 0xE004);
}

/// PLOAD of a page whose descriptor was invalidated drops the stale ATC
/// entry before the search (M68030UM 9.8), so the next access faults.
#[test]
fn pload_of_an_invalidated_page_drops_its_old_entry() {
    let mut cpu = cpu_mmu_030();
    assert_eq!(data(&mut cpu, 0x9004), 0x9004);
    write_long(&mut cpu.bus, TABLE + 9 * 4, 0);
    assert_eq!(data(&mut cpu, 0x9004), 0x9004);

    cpu.regs.a[0] = 0x9000;
    run(&mut cpu, &[0xF010, 0x2215]); // PLOADR #5,(A0)
    let err = cpu
        .pmmu_translate(FC_SUPERVISOR_DATA, 0x9004, false)
        .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<CpuError>(),
        Some(CpuError::BusError(_))
    ));
}

#[test]
fn pload_fills_the_entry_ahead_of_the_access() {
    let mut cpu = cpu_mmu_030();
    // PLOADR #5,(A0)
    cpu.regs.a[0] = 0x8000;
    run(&mut cpu, &[0xF010, 0x2215]);
    cpu.bus.reset_trace();
    assert_eq!(data(&mut cpu, 0x8010), 0x8010);
    assert_eq!(table_reads(&cpu), 0, "loaded by PLOAD");
}

#[test]
fn write_protected_page_hits_for_reads_and_faults_writes() {
    let mut cpu = cpu_mmu_030();
    // WP on page 3.
    write_long(&mut cpu.bus, TABLE + 3 * 4, (3 << 12) | 1 | 4);

    assert_eq!(data(&mut cpu, 0x3000), 0x3000);
    let err = cpu
        .pmmu_translate(FC_SUPERVISOR_DATA, 0x3000, true)
        .unwrap_err();
    assert!(
        matches!(err.downcast_ref::<CpuError>(), Some(CpuError::BusError(_))),
        "{err:?}"
    );
    // Still faults on a second write, and reads still hit.
    assert!(
        cpu.pmmu_translate(FC_SUPERVISOR_DATA, 0x3004, true)
            .is_err()
    );
    cpu.bus.reset_trace();
    assert_eq!(data(&mut cpu, 0x3008), 0x3008);
    assert_eq!(table_reads(&cpu), 0);
}

/// M68030UM 9.7.5: RESET clears the E bits of the TC and TTx registers but
/// does NOT flush the ATC; software flushes explicitly (the PMOVE that
/// re-enables translation flushes when FD=0). The upstream vector, which
/// expects an empty ATC after reset, deviates from the hardware and is
/// corrected here.
#[test]
fn reset_disables_translation_without_flushing_the_atc() {
    let mut cpu = cpu_mmu_030();
    data(&mut cpu, 0x3000);
    assert!(
        cpu.pmmu_atc_lookup(PMMU_ATC_URP, 3).is_some(),
        "entry cached before reset"
    );

    cpu.reset().expect("reset");

    assert!(!cpu.regs.pmmu.tc.enable(), "RESET clears the TC E bit");
    assert!(
        cpu.regs.pmmu.tt.iter().all(|tt| !tt.e()),
        "RESET clears the TTx E bits"
    );
    assert!(
        cpu.pmmu_atc_lookup(PMMU_ATC_URP, 3).is_some(),
        "RESET does not flush the ATC (M68030UM 9.7.5)"
    );
}

#[test]
fn atc_hit_write_sets_leaf_descriptor_m_bit() {
    // First write through a resident page sets the leaf descriptor's M bit
    // via the descriptor address recorded in the ATC entry.
    let mut cpu = cpu_mmu_030();
    assert_eq!(data(&mut cpu, 0x5004), 0x5004);
    cpu.pmmu_translate(FC_SUPERVISOR_DATA, 0x5004, true)
        .expect("write");

    let desc = cpu.pmmu_atc_lookup(PMMU_ATC_URP, 5).expect("entry");
    assert!(desc.modified, "ATC entry promoted to modified");

    let leaf = desc.leaf_desc_addr;
    let val = (cpu.bus.mem.get(&leaf).copied().unwrap_or(0) as Long) << 24
        | (cpu.bus.mem.get(&(leaf + 1)).copied().unwrap_or(0) as Long) << 16
        | (cpu.bus.mem.get(&(leaf + 2)).copied().unwrap_or(0) as Long) << 8
        | cpu.bus.mem.get(&(leaf + 3)).copied().unwrap_or(0) as Long;
    // DT=1 | U (set by the first walk) | M (set by the first write).
    assert_eq!(
        val,
        (5 << 12) | 1 | (1 << 3) | (1 << 4),
        "M bit set in the leaf descriptor"
    );
}

/// MC68851 PFLUSH forms (MC68851 UM A-6..A-7), which also run on Snow's
/// 68020 PMMU. The 68851 mask is 4 bits in bits 8-5 and modes 101/111 are
/// PFLUSHS, which also flushes globally shared entries.
mod m68851 {
    use super::*;
    use crate::cpu_m68k::{CpuM68020Pmmu, M68020_ADDRESS_MASK};

    type TestCpu = CpuM68020Pmmu<TestBus>;

    /// A supervisor-mode 68020 with the 68851 enabled over the same
    /// identity-mapped table as `cpu_mmu_030`.
    fn cpu_68851() -> TestCpu {
        let bus = Testbus::new(M68020_ADDRESS_MASK);
        let mut cpu = TestCpu::new(bus);
        cpu.regs.sr.set_supervisor(true);
        cpu.regs.sr.set_int_prio_mask(7);
        for page in 0..32 {
            map(&mut cpu.bus, page, page);
        }
        let mut crp = RootPointerReg(0);
        crp.set_table_addr(TABLE >> 4);
        crp.set_dt(2);
        crp.set_limit(0x7FFF);
        cpu.regs.pmmu.crp = crp;
        cpu.regs.pmmu.tc = TcReg(TC);
        cpu.pmmu_cache_ensure();
        cpu
    }

    fn data(cpu: &mut TestCpu, logical: Address) -> Address {
        cpu.pmmu_translate(FC_SUPERVISOR_DATA, logical, false)
            .expect("translation must succeed")
    }

    fn run(cpu: &mut TestCpu, words: &[Word]) {
        for (i, &w) in words.iter().enumerate() {
            write_word(&mut cpu.bus, CODE + i as Address * 2, w);
        }
        cpu.set_pc(CODE).expect("set_pc");
        cpu.prefetch_refill().expect("prefetch");
        cpu.step().expect("step");
    }

    /// PFLUSHS (mode 101) must flush everything, shared entries included.
    /// Snow's ATC does not track the SG bit, so it over-flushes, which the
    /// hardware model allows.
    #[test]
    fn pflushs_flushes_shared_entries() {
        let mut cpu = cpu_68851();
        assert_eq!(data(&mut cpu, 0x5004), 0x5004);
        map(&mut cpu.bus, 5, 0x9);
        assert_eq!(data(&mut cpu, 0x5004), 0x5004, "stale entry held");

        run(&mut cpu, &[0xF000, 0x3400]); // PFLUSHS
        assert_eq!(data(&mut cpu, 0x5004), 0x9004, "shared entry flushed");
    }

    /// The 68851's 4-bit mask lives in bits 8-5; a form with mask bit 3 set
    /// must still dispatch as PFLUSH rather than taking the F-line path.
    #[test]
    fn pflush_68851_four_bit_mask_decodes() {
        let mut cpu = cpu_68851();
        assert_eq!(data(&mut cpu, 0x5004), 0x5004);
        map(&mut cpu.bus, 5, 0x9);
        assert_eq!(data(&mut cpu, 0x5004), 0x5004, "stale entry held");

        cpu.regs.a[0] = 0x5000;
        run(&mut cpu, &[0xF010, 0x3915]); // PFLUSH #5,#8,(A0): mask bit 3
        assert_eq!(data(&mut cpu, 0x5004), 0x9004, "entry flushed");
    }
}

/// PTEST level 0 searches the ATC only (M68000PRM 6-62): a cached entry is
/// reported with its WP/M status, an uncached address reports I.
#[test]
fn ptest_level_0_searches_the_atc() {
    let mut cpu = cpu_mmu_030();
    // Cache page 3 with a read, then a write so the entry is modified.
    assert_eq!(data(&mut cpu, 0x3000), 0x3000);
    cpu.pmmu_translate(FC_SUPERVISOR_DATA, 0x3000, true)
        .expect("write");

    cpu.regs.a[0] = 0x3000;
    run(&mut cpu, &[0xF010, 0x8215]); // PTESTR #5,(A0),#0
    assert_eq!(
        cpu.regs.pmmu.psr.0, 0x0200,
        "resident, modified entry: M set, no I"
    );

    // An uncached address reports I.
    cpu.regs.a[0] = 0x9000;
    run(&mut cpu, &[0xF010, 0x8215]);
    assert_eq!(cpu.regs.pmmu.psr.0, 0x0400, "not resident: I set");

    // A write-protected page reports W.
    write_long(&mut cpu.bus, TABLE + 4 * 4, (4 << 12) | 1 | 4); // page 4 WP
    assert_eq!(data(&mut cpu, 0x4000), 0x4000, "walk caches the WP entry");
    cpu.regs.a[0] = 0x4000;
    run(&mut cpu, &[0xF010, 0x8215]);
    assert_eq!(cpu.regs.pmmu.psr.0, 0x0800, "write-protected entry: W set");
}

/// PTEST level 0 reports a transparent-translation match in MMUSR T
/// (M68030UM Table 9-3).
#[test]
fn ptest_level_0_reports_transparent_translation() {
    let mut cpu = cpu_mmu_030();
    // TT0 matches every supervisor-data access (RW masked).
    let mut tt = TrTranslationReg(0);
    tt.set_e(true);
    tt.set_fc_base(FC_SUPERVISOR_DATA & 0b111);
    tt.set_fc_mask(0);
    tt.set_rwm(true);
    cpu.regs.pmmu.tt[0] = tt;

    cpu.regs.a[0] = 0x3000;
    run(&mut cpu, &[0xF010, 0x8215]); // PTESTR #5,(A0),#0
    assert_eq!(cpu.regs.pmmu.psr.0, 0x0040, "T set on TT match");
}

/// PTEST level 0 with an address-register operand takes an F-line
/// unimplemented instruction exception (M68000PRM 6-63).
#[test]
fn ptest_level_0_with_a_register_is_f_line() {
    const HANDLER: Address = 0x0300;

    let mut cpu = cpu_mmu_030();
    cpu.regs.isp = 0x1000; // supervisor stack for the exception frame
    write_long(&mut cpu.bus, 11 * 4, HANDLER); // vector 11: F-line emulator
    cpu.regs.a[0] = 0x3000;
    run(&mut cpu, &[0xF010, 0x8315]); // PTESTR #5,(A0),#0,A0
    assert_eq!(cpu.regs.pc, HANDLER, "F-line exception taken");
}
