//! Tests for the M68040 MMU

use crate::bus::Address;
use crate::bus::access::Bus68020;
use crate::bus::testbus::Testbus;
use crate::cpu_m68k::regs::Register;
use crate::cpu_m68k::{CpuM68040, FPU_M68040, M68040_ADDRESS_MASK};
use crate::types::{Field32, Long};

type TestCpu = CpuM68040<Testbus<Address, u8, Bus68020>>;

// Physical addresses of the three table levels
const ROOT_TABLE: Address = 0x0001_0000;
const PTR_TABLE: Address = 0x0001_1000;
const PAGE_TABLE: Address = 0x0001_2000;

// Function codes
const FC_SUPER_DATA: u8 = 5;
const FC_USER_DATA: u8 = 1;
const FC_SUPER_PROGRAM: u8 = 6;

// Descriptor bits
const RESIDENT: Long = 1;
const TABLE_VALID: Long = 2;
const WP: Long = 1 << 2;
const SUPERVISOR: Long = 1 << 7;

fn write_long(cpu: &mut TestCpu, addr: Address, value: Long) {
    for i in 0..4 {
        cpu.bus.mem.insert(addr + i, (value >> (24 - i * 8)) as u8);
    }
}

fn read_long(cpu: &TestCpu, addr: Address) -> Long {
    let byte = |offset: Address| *cpu.bus.mem.get(&(addr + offset)).unwrap_or(&0);

    let mut value = Field32(0);
    value.set_be0(byte(0));
    value.set_be1(byte(1));
    value.set_be2(byte(2));
    value.set_be3(byte(3));
    value.0
}

/// Logical address used by the tests: root index 8, pointer index 33, page index 1
const LOGICAL: Address = 0x1084_2000;

fn testcpu(leaf_flags: Long) -> TestCpu {
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK), FPU_M68040);

    // Create a page table that points LOGICAL to 0x200000
    write_long(&mut cpu, ROOT_TABLE + 8 * 4, PTR_TABLE | TABLE_VALID);
    write_long(&mut cpu, PTR_TABLE + 33 * 4, PAGE_TABLE | TABLE_VALID);
    write_long(
        &mut cpu,
        PAGE_TABLE + 4,
        0x0020_0000 | RESIDENT | leaf_flags,
    );

    cpu.regs.write(Register::SRP040, ROOT_TABLE);
    cpu.regs.write(Register::URP040, ROOT_TABLE);
    // Enable, 8KB pages
    cpu.regs.write(Register::TC040, 0xC000_u32);
    cpu.pmmu_cache_ensure();
    cpu
}

#[test]
fn translate_page() {
    let mut cpu = testcpu(0);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false)
            .unwrap()
            .paddr,
        0x0020_0000
    );
    // Offset within the page is preserved
    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL + 0x1ABC, false)
            .unwrap()
            .paddr,
        0x0020_1ABC
    );
    // next page is not mapped
    assert!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL + 0x2000, false)
            .is_err()
    );
}

#[test]
fn translate_disabled_is_identity() {
    let mut cpu = testcpu(0);
    cpu.regs.write(Register::TC040, 0_u32);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false)
            .unwrap()
            .paddr,
        LOGICAL
    );
}

#[test]
fn walk_sets_used_and_modified() {
    let mut cpu = testcpu(0);

    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
    assert_eq!(
        read_long(&cpu, ROOT_TABLE + 8 * 4) & (1 << 3),
        1 << 3,
        "root U"
    );
    assert_eq!(
        read_long(&cpu, PTR_TABLE + 33 * 4) & (1 << 3),
        1 << 3,
        "pointer U"
    );
    assert_eq!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 3), 1 << 3, "page U");
    assert_eq!(
        read_long(&cpu, PAGE_TABLE + 4) & (1 << 4),
        0,
        "M not set on read"
    );

    // Test M-bit on leaf at write
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).unwrap();
    assert_eq!(
        read_long(&cpu, PAGE_TABLE + 4) & (1 << 4),
        1 << 4,
        "M set on write"
    );
}

#[test]
fn write_protect() {
    // On the leaf
    let mut cpu = testcpu(WP);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).is_err());

    // Inherited from a table descriptor higher up
    let mut cpu = testcpu(0);
    write_long(&mut cpu, PTR_TABLE + 33 * 4, PAGE_TABLE | TABLE_VALID | WP);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).is_err());
}

#[test]
fn supervisor_only_page() {
    let mut cpu = testcpu(SUPERVISOR);

    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_USER_DATA, LOGICAL, false).is_err());
}

#[test]
fn indirect_descriptor() {
    let mut cpu = testcpu(0);
    // Leaf points at another descriptor at $13000
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0001_3000 | 2);
    write_long(&mut cpu, 0x0001_3000, 0x0030_0000 | RESIDENT);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false)
            .unwrap()
            .paddr,
        0x0030_0000
    );

    // Two levels of indirection are invalid
    let mut cpu = testcpu(0);
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0001_3000 | 2);
    write_long(&mut cpu, 0x0001_3000, 0x0001_4000 | 2);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_err());
}

#[test]
fn transparent_translation() {
    let mut cpu = testcpu(0);
    // Data TT: $F9000000-$F9FFFFFF, both user and supervisor
    cpu.regs.write(Register::DTT0, 0xF900_C060_u32);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, 0xF900_1234, false)
            .unwrap()
            .paddr,
        0xF900_1234
    );
    // Program access not translated (because DTT0)
    assert!(
        cpu.pmmu_translate(FC_SUPER_PROGRAM, 0xF900_1234, false)
            .is_err()
    );
}

#[test]
fn transparent_translation_write_protect() {
    let mut cpu = testcpu(0);
    cpu.regs.write(Register::DTT0, 0xF900_C064_u32);

    assert!(
        cpu.pmmu_translate(FC_SUPER_DATA, 0xF900_1234, false)
            .is_ok()
    );
    assert!(
        cpu.pmmu_translate(FC_SUPER_DATA, 0xF900_1234, true)
            .is_err()
    );
}

#[test]
fn separate_root_pointers() {
    let mut cpu = testcpu(0);
    // Valid URP, but empty table
    cpu.regs.write(Register::URP040, 0x0002_0000_u32);
    cpu.pmmu_cache_invalidate();

    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_ok());
    assert!(cpu.pmmu_translate(FC_USER_DATA, LOGICAL, false).is_err());
}

#[test]
fn table_descriptor_type_01_is_invalid() {
    let mut cpu = testcpu(0);
    write_long(&mut cpu, PTR_TABLE + 33 * 4, PAGE_TABLE | 1);

    assert!(cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).is_err());
    assert_eq!(
        read_long(&cpu, PTR_TABLE + 33 * 4) & (1 << 3),
        0,
        "U not set on an invalid descriptor"
    );
}

#[test]
fn indirect_descriptor_not_updated() {
    const INDIRECT: Address = 0x0001_3000;
    let mut cpu = testcpu(0);
    let leaf = read_long(&cpu, PAGE_TABLE + 4);
    write_long(&mut cpu, INDIRECT, leaf);
    write_long(&mut cpu, PAGE_TABLE + 4, INDIRECT | 2);

    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true)
            .unwrap()
            .paddr,
        0x0020_0000
    );
    assert_eq!(
        read_long(&cpu, PAGE_TABLE + 4),
        INDIRECT | 2,
        "indirect descriptor untouched"
    );
    assert_eq!(
        read_long(&cpu, INDIRECT) & (3 << 3),
        3 << 3,
        "U and M on the page descriptor"
    );
}

#[test]
fn pmmu_opcodes_are_line_f() {
    use crate::cpu_m68k::instruction::{Instruction, InstructionMnemonic};
    use crate::cpu_m68k::{M68030, M68040};

    // PMOVE (A0),TC
    assert_eq!(
        Instruction::try_decode(M68030, 0xF010).unwrap().mnemonic,
        InstructionMnemonic::POP_000
    );
    assert_eq!(
        Instruction::try_decode(M68040, 0xF010).unwrap().mnemonic,
        InstructionMnemonic::LINEF
    );

    // Taken as the F-line exception
    let mut cpu = TestCpu::new(Testbus::new(M68040_ADDRESS_MASK), FPU_M68040);
    write_long(&mut cpu, 0x1000, 0xF010_4000);
    write_long(&mut cpu, 0x2C, 0x2000);
    cpu.regs.isp = 0x8000;
    cpu.regs.sr.set_supervisor(true);
    cpu.set_pc(0x1000).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, 0x2000);
    assert_eq!(read_long(&cpu, 0x8000 - 6), 0x1000);
}

/// The 8 KB page `k` pages of an ATC set away from LOGICAL: they all go in
/// the same set (MC68040UM 3.3), LOGICAL being k = 0
fn same_set(k: Address) -> Address {
    LOGICAL + k * 0x2_0000
}

/// Maps a page of root entry 8, as LOGICAL is, to `paddr`: page tables
/// follow PAGE_TABLE, one per pointer entry from 33 on
fn map(cpu: &mut TestCpu, laddr: Address, paddr: Address) {
    assert_eq!(laddr >> 25, 8);
    let ptr = (laddr >> 18) & 0x7F;
    let table = PAGE_TABLE + (ptr - 33) * 0x100;
    write_long(cpu, PTR_TABLE + ptr * 4, table | TABLE_VALID);
    write_long(cpu, table + ((laddr >> 13) & 0x1F) * 4, paddr | RESIDENT);
}

/// Whether the data ATC holds the supervisor page at `laddr`
fn in_data_atc(cpu: &TestCpu, laddr: Address) -> bool {
    use crate::cpu_m68k::pmmu::atc040::{ATC040_DATA, Atc040};
    cpu.atc040[ATC040_DATA]
        .peek(Atc040::tag(laddr, 0x1FFF, true), 13)
        .is_some()
}

/// Translates pages of the set of LOGICAL until LOGICAL is replaced
fn replace_logical(cpu: &mut TestCpu) {
    for k in 1..=12 {
        map(cpu, same_set(k), 0x0040_0000 + k * 0x2000);
        cpu.pmmu_translate(FC_SUPER_DATA, same_set(k), false)
            .unwrap();
        if !in_data_atc(cpu, LOGICAL) {
            return;
        }
    }
    panic!("LOGICAL never replaced");
}

/// Runs one instruction with the code and the stack transparently
/// translated
fn run(cpu: &mut TestCpu, opcode: u16) {
    const CODE: Address = 0x1000;
    // The first 16 MB, supervisor and user
    cpu.regs.write(Register::ITT0, 0x0000_C000_u32);
    cpu.regs.write(Register::DTT0, 0x0000_C000_u32);
    cpu.bus.mem.insert(CODE, (opcode >> 8) as u8);
    cpu.bus.mem.insert(CODE + 1, opcode as u8);
    cpu.regs.isp = 0x8000;
    cpu.regs.sr.set_supervisor(true);
    cpu.set_pc(CODE).unwrap();
    cpu.prefetch_refill().unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.regs.pc, CODE + 2);
}

#[test]
fn atc_set_holds_four_pages() {
    let mut cpu = testcpu(0);
    for k in 0..5 {
        map(&mut cpu, same_set(k), 0x0040_0000 + k * 0x2000);
        cpu.pmmu_translate(FC_SUPER_DATA, same_set(k), false)
            .unwrap();
    }
    let held = (0..5).filter(|&k| in_data_atc(&cpu, same_set(k))).count();
    assert_eq!(held, 4);
}

#[test]
fn modified_bit_after_replacement() {
    // Mac OS 8.1 VM clears M in a page descriptor without flushing the ATC.
    // Writes don't set M again while the ATC holds the page as modified,
    // but do once the entry was replaced.
    let mut cpu = testcpu(0);
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).unwrap();
    assert_ne!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 4), 0);

    write_long(&mut cpu, PAGE_TABLE + 4, 0x0020_0000 | RESIDENT);
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).unwrap();
    assert_eq!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 4), 0);

    replace_logical(&mut cpu);
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).unwrap();
    assert_ne!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 4), 0);
}

#[test]
fn write_to_unmodified_entry_searches_again() {
    let mut cpu = testcpu(0);
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0030_0000 | RESIDENT);

    // Reads keep using the entry, a write searches the tables again
    let tr = cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
    assert_eq!(tr.paddr, 0x0020_0000);
    let tr = cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, true).unwrap();
    assert_eq!(tr.paddr, 0x0030_0000);
    assert_ne!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 4), 0);
}

#[test]
fn invalid_page_faults_until_flushed() {
    let mut cpu = testcpu(0);
    let laddr = LOGICAL + 0x2000;
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, laddr, false).is_err());

    // The ATC holds the failed search
    write_long(&mut cpu, PAGE_TABLE + 8, 0x0030_0000 | RESIDENT);
    assert!(cpu.pmmu_translate(FC_SUPER_DATA, laddr, false).is_err());

    // PFLUSH (A0), supervisor
    cpu.regs.write_a(0, laddr);
    cpu.regs.dfc = 5;
    run(&mut cpu, 0xF508);
    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, laddr, false)
            .unwrap()
            .paddr,
        0x0030_0000
    );
}

#[test]
fn pflush_variants() {
    const GLOBAL: Long = 1 << 10;
    let next = LOGICAL + 0x2000;
    let fill = |cpu: &mut TestCpu| {
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
        cpu.pmmu_translate(FC_SUPER_DATA, next, false).unwrap();
    };
    let mut cpu = testcpu(GLOBAL);
    write_long(&mut cpu, PAGE_TABLE + 8, 0x0030_0000 | RESIDENT);
    cpu.regs.write_a(0, LOGICAL);

    // PFLUSH (A0) with a user DFC keeps the supervisor entries
    fill(&mut cpu);
    cpu.regs.dfc = 1;
    run(&mut cpu, 0xF508);
    assert!(in_data_atc(&cpu, LOGICAL) && in_data_atc(&cpu, next));

    // PFLUSHN (A0) keeps a global entry, PFLUSH (A0) does not
    cpu.regs.dfc = 5;
    run(&mut cpu, 0xF500);
    assert!(in_data_atc(&cpu, LOGICAL));
    run(&mut cpu, 0xF508);
    assert!(!in_data_atc(&cpu, LOGICAL) && in_data_atc(&cpu, next));

    // PFLUSHAN keeps the global entries, PFLUSHA none
    fill(&mut cpu);
    run(&mut cpu, 0xF510);
    assert!(in_data_atc(&cpu, LOGICAL) && !in_data_atc(&cpu, next));
    run(&mut cpu, 0xF518);
    assert!(!in_data_atc(&cpu, LOGICAL));
}

#[test]
fn ptest_reloads_the_entry() {
    let mut cpu = testcpu(0);
    cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
    write_long(&mut cpu, PAGE_TABLE + 4, 0x0030_0000 | RESIDENT);

    // PTESTR (A0) loads the new translation, PTESTW also sets M
    cpu.regs.write_a(0, LOGICAL);
    cpu.regs.dfc = 5;
    run(&mut cpu, 0xF568);
    assert_eq!(cpu.regs.mmu040.mmusr & !0xFFF, 0x0030_0000);
    assert_eq!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 4), 0);
    assert_eq!(
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false)
            .unwrap()
            .paddr,
        0x0030_0000
    );
    run(&mut cpu, 0xF548);
    assert_ne!(read_long(&cpu, PAGE_TABLE + 4) & (1 << 4), 0);
    assert_ne!(cpu.regs.mmu040.mmusr & (1 << 4), 0);
}

#[test]
fn replaced_page_leaves_the_last_translations() {
    // Accesses reuse the last translation made in a page: not once the ATC
    // replaced the page. The second half of LOGICAL has last translations
    // of its own, which the other pages of the set don't overwrite.
    let mut cpu = testcpu(0);
    cpu.regs.sr.set_supervisor(true);
    let laddr = LOGICAL + 0x1000;
    write_long(&mut cpu, 0x0020_1000, 0x1122_3344);
    write_long(&mut cpu, 0x0030_1000, 0x5566_7788);
    assert_eq!(cpu.read_ticks::<Long>(laddr).unwrap(), 0x1122_3344);

    write_long(&mut cpu, PAGE_TABLE + 4, 0x0030_0000 | RESIDENT);
    assert_eq!(cpu.read_ticks::<Long>(laddr).unwrap(), 0x1122_3344);
    replace_logical(&mut cpu);
    assert_eq!(cpu.read_ticks::<Long>(laddr).unwrap(), 0x5566_7788);
}

#[test]
fn reloaded_entry_leaves_the_last_translations() {
    // A write to a page not marked modified searches the tables again and
    // reloads the entry: reads then use the new translation too
    let mut cpu = testcpu(0);
    cpu.regs.sr.set_supervisor(true);
    write_long(&mut cpu, 0x0020_0000, 0x1122_3344);
    write_long(&mut cpu, 0x0030_0000, 0x5566_7788);
    assert_eq!(cpu.read_ticks::<Long>(LOGICAL).unwrap(), 0x1122_3344);

    write_long(&mut cpu, PAGE_TABLE + 4, 0x0030_0000 | RESIDENT);
    cpu.write_ticks::<Long>(LOGICAL + 4, 0).unwrap();
    assert_eq!(cpu.read_ticks::<Long>(LOGICAL).unwrap(), 0x5566_7788);
}

#[test]
fn vm_pflush_workaround() {
    // A user-mode PFLUSHA takes a privilege violation, or flushes with the
    // workaround
    for workaround in [false, true] {
        let mut cpu = testcpu(0);
        cpu.set_vm_pflush_workaround(workaround);
        cpu.pmmu_translate(FC_SUPER_DATA, LOGICAL, false).unwrap();
        cpu.regs.write(Register::ITT0, 0x0000_C000_u32);
        cpu.regs.write(Register::DTT0, 0x0000_C000_u32);
        cpu.bus.mem.insert(0x1000, 0xF5);
        cpu.bus.mem.insert(0x1001, 0x18);
        write_long(&mut cpu, 0x20, 0x2000);
        cpu.regs.isp = 0x8000;
        cpu.regs.sr.set_supervisor(false);
        cpu.set_pc(0x1000).unwrap();
        cpu.prefetch_refill().unwrap();
        cpu.step().unwrap();
        if workaround {
            assert_eq!(cpu.regs.pc, 0x1002);
            assert!(!in_data_atc(&cpu, LOGICAL));
        } else {
            assert_eq!(cpu.regs.pc, 0x2000);
            assert!(in_data_atc(&cpu, LOGICAL));
        }
    }
}
